//! Push delivery of due inbox reminders.
//!
//! Opt-in: reminders are pushed only to one explicitly configured chat
//! (`LIA_REMINDER_CHANNEL` and `LIA_REMINDER_CONVERSATION`), never broadcast.
//! A reminder is marked notified only after the send succeeded, so a crash
//! can repeat a reminder but never silently drop one. Changing a reminder's
//! due time or reopening it clears the mark.

use chrono::{DateTime, Utc};

use crate::channel::OutboundMessage;
use crate::inbox::{InboxStore, Kind, Owner, Record, State, Update};
use crate::routine::{ChannelLookup, RoutineTarget};

/// Most reminders pushed in one pass, so a backlog cannot flood a chat.
pub const MAX_PER_PASS: usize = 10;

/// The reminders that are due now and not yet pushed.
pub fn due_reminders(store: &InboxStore, owner: &Owner, now: DateTime<Utc>) -> Vec<Record> {
    let mut due: Vec<Record> = store
        .list(owner, &Default::default())
        .into_iter()
        .filter(|r| {
            r.kind == Kind::Reminder
                && r.state == State::Open
                && r.notified_at.is_none()
                && r.due_at.is_some_and(|d| d <= now)
                && r.snoozed_until.is_none_or(|s| s <= now)
        })
        .collect();
    due.sort_by_key(|r| (r.due_at, r.created_at));
    due.truncate(MAX_PER_PASS);
    due
}

/// Push each due reminder to `target` and mark it notified. Returns how many
/// were delivered. The reminder text is user data and is sent verbatim.
pub async fn deliver_due(
    store: &mut InboxStore,
    owner: &Owner,
    now: DateTime<Utc>,
    target: &RoutineTarget,
    channel: ChannelLookup<'_>,
) -> usize {
    let Some(adapter) = channel(&target.channel) else {
        return 0;
    };
    let mut delivered = 0;
    for record in due_reminders(store, owner, now) {
        let message = OutboundMessage::new(
            target.conversation.clone(),
            format!("Reminder: {}", record.text),
        );
        if let Err(error) = adapter.send(&message).await {
            tracing::warn!("could not deliver a reminder: {error}");
            break;
        }
        let update = Update {
            notified_at: Some(Some(now)),
            ..Update::default()
        };
        if let Err(error) = store.update(owner, record.id, update) {
            tracing::warn!("could not mark a reminder delivered: {error}");
            break;
        }
        delivered += 1;
    }
    delivered
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Duration;

    use super::*;
    use crate::channel::{Channel, InboundSender, OutboundSender};
    use crate::inbox::{NewRecord, Provenance};

    struct Recorder {
        sent: Mutex<Vec<OutboundMessage>>,
    }

    #[async_trait]
    impl Channel for Recorder {
        fn name(&self) -> &str {
            "test"
        }
        async fn run(&self, _: InboundSender, _: OutboundSender) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send(&self, message: &OutboundMessage) -> anyhow::Result<()> {
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }
    }

    fn reminder(store: &mut InboxStore, owner: &Owner, text: &str, due: DateTime<Utc>) -> Record {
        let mut new = NewRecord::new(Kind::Reminder, text, Provenance::User);
        new.due_at = Some(due);
        store.create(owner, new).unwrap()
    }

    #[tokio::test]
    async fn a_due_reminder_is_pushed_once_and_a_new_due_time_rearms_it() {
        let owner = Owner::new("local").unwrap();
        let mut store = InboxStore::in_memory();
        let now = Utc::now();
        let due = reminder(&mut store, &owner, "stand up", now - Duration::minutes(1));
        reminder(&mut store, &owner, "later", now + Duration::hours(1));
        let mut task = NewRecord::new(Kind::Task, "not a reminder", Provenance::User);
        task.due_at = Some(now - Duration::minutes(1));
        store.create(&owner, task).unwrap();

        let recorder = Arc::new(Recorder {
            sent: Mutex::new(Vec::new()),
        });
        let adapter = recorder.clone();
        let lookup = move |name: &str| -> Option<Arc<dyn Channel>> {
            (name == "test").then(|| adapter.clone() as Arc<dyn Channel>)
        };
        let target = RoutineTarget {
            channel: "test".into(),
            conversation: "chat".into(),
        };

        assert_eq!(
            deliver_due(&mut store, &owner, now, &target, &lookup).await,
            1
        );
        assert_eq!(
            deliver_due(&mut store, &owner, now, &target, &lookup).await,
            0
        );
        {
            let sent = recorder.sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert!(sent[0].text.contains("stand up"));
        }

        let update = Update {
            due_at: Some(Some(now - Duration::seconds(30))),
            ..Update::default()
        };
        store.update(&owner, due.id, update).unwrap();
        assert_eq!(
            deliver_due(&mut store, &owner, now, &target, &lookup).await,
            1
        );
    }

    #[tokio::test]
    async fn an_unavailable_channel_delivers_nothing_and_keeps_the_reminder_due() {
        let owner = Owner::new("local").unwrap();
        let mut store = InboxStore::in_memory();
        let now = Utc::now();
        reminder(&mut store, &owner, "stand up", now - Duration::minutes(1));
        let lookup = |_: &str| -> Option<Arc<dyn Channel>> { None };
        let target = RoutineTarget {
            channel: "missing".into(),
            conversation: "chat".into(),
        };
        assert_eq!(
            deliver_due(&mut store, &owner, now, &target, &lookup).await,
            0
        );
        assert_eq!(due_reminders(&store, &owner, now).len(), 1);
    }
}
