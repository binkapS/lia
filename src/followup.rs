//! Confirmed follow-ups: user-scoped tasks kept in the [`InboxStore`].
//!
//! A follow-up is only created through [`FollowUps::capture`], which needs a
//! [`Confirmation`] and stores the record with [`Provenance::User`]. Nothing
//! here infers, routes or sends anything; a model-suggested item stays a
//! [`Draft`] until the user explicitly confirms it. The caller supplies the
//! [`Owner`] on every call; other owners' records, and records that are not
//! tasks, are reported as not found. The store keeps due times in UTC only, so
//! callers convert from the user's timezone before setting `due_at`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::inbox::{
    Filter, InboxError, InboxStore, Kind, NewRecord, Owner, Priority, Provenance, Record, Result,
    Source, State, Update,
};

/// Proof that the user explicitly asked to keep a follow-up.
#[derive(Debug, Clone, Copy)]
pub struct Confirmation(());

impl Confirmation {
    /// Call only after the user has explicitly confirmed this follow-up.
    pub fn user_confirmed() -> Confirmation {
        Confirmation(())
    }
}

/// A proposed follow-up. Holding one stores nothing.
#[derive(Debug, Clone)]
pub struct Draft {
    pub text: String,
    pub priority: Priority,
    pub due_at: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub message_id: Option<String>,
}

impl Draft {
    pub fn new(text: impl Into<String>) -> Draft {
        Draft {
            text: text.into(),
            priority: Priority::Normal,
            due_at: None,
            session_id: None,
            message_id: None,
        }
    }

    /// Record the session and message the follow-up came from.
    pub fn from_message(
        mut self,
        session_id: impl Into<String>,
        message_id: impl Into<String>,
    ) -> Draft {
        self.session_id = Some(session_id.into());
        self.message_id = Some(message_id.into());
        self
    }
}

/// Follow-up operations over an inbox store.
#[derive(Debug)]
pub struct FollowUps {
    store: InboxStore,
}

impl FollowUps {
    pub fn new(store: InboxStore) -> FollowUps {
        FollowUps { store }
    }

    pub fn store(&self) -> &InboxStore {
        &self.store
    }

    /// Store a confirmed follow-up for `owner`.
    pub fn capture(
        &mut self,
        owner: &Owner,
        draft: Draft,
        _confirmation: Confirmation,
    ) -> Result<Record> {
        let mut new = NewRecord::new(Kind::Task, draft.text, Provenance::User);
        new.priority = draft.priority;
        new.due_at = draft.due_at;
        new.source = Source {
            session_id: draft.session_id,
            message_id: draft.message_id,
            channel: None,
        };
        self.store.create(owner, new)
    }

    pub fn get(&self, owner: &Owner, id: Uuid) -> Result<Record> {
        let record = self.store.get(owner, id)?;
        if record.kind == Kind::Task {
            Ok(record)
        } else {
            Err(InboxError::NotFound)
        }
    }

    /// The owner's follow-ups in `state`; `None` lists all but archived.
    pub fn list(&self, owner: &Owner, state: Option<State>) -> Vec<Record> {
        self.store.list(
            owner,
            &Filter {
                kind: Some(Kind::Task),
                state,
                include_archived: false,
            },
        )
    }

    /// Open follow-ups that are not deferred past `now`.
    pub fn list_open(&self, owner: &Owner, now: DateTime<Utc>) -> Vec<Record> {
        self.list(owner, Some(State::Open))
            .into_iter()
            .filter(|r| r.snoozed_until.is_none_or(|t| t <= now))
            .collect()
    }

    pub fn complete(&mut self, owner: &Owner, id: Uuid) -> Result<Record> {
        self.apply(owner, id, |u| u.state = Some(State::Done))
    }

    /// Reopen a completed or archived follow-up.
    pub fn reopen(&mut self, owner: &Owner, id: Uuid) -> Result<Record> {
        self.apply(owner, id, |u| u.state = Some(State::Open))
    }

    /// Hide an open follow-up until `until`, which must be in the future.
    pub fn defer(&mut self, owner: &Owner, id: Uuid, until: DateTime<Utc>) -> Result<Record> {
        if until <= Utc::now() {
            return Err(InboxError::Invalid("deferral must be in the future"));
        }
        self.apply(owner, id, |u| u.snoozed_until = Some(Some(until)))
    }

    pub fn archive(&mut self, owner: &Owner, id: Uuid) -> Result<Record> {
        self.apply(owner, id, |u| u.state = Some(State::Archived))
    }

    fn apply(&mut self, owner: &Owner, id: Uuid, set: impl FnOnce(&mut Update)) -> Result<Record> {
        self.get(owner, id)?;
        let mut update = Update::default();
        set(&mut update);
        self.store.update(owner, id, update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn owner(id: &str) -> Owner {
        Owner::new(id).unwrap()
    }

    #[test]
    fn drafting_stores_nothing_and_capture_is_user_task() {
        let mut f = FollowUps::new(InboxStore::in_memory());
        let me = owner("me");
        let draft = Draft::new("call Sam");
        assert!(f.list(&me, None).is_empty());
        let r = f
            .capture(&me, draft, Confirmation::user_confirmed())
            .unwrap();
        assert_eq!(r.kind, Kind::Task);
        assert_eq!(r.provenance, Provenance::User);
        assert_eq!(f.list(&me, None).len(), 1);
    }

    #[test]
    fn source_is_traceable_and_owner_access_spans_sessions() {
        let mut f = FollowUps::new(InboxStore::in_memory());
        let me = owner("me");
        let due = Utc::now() + Duration::days(1);
        let mut d = Draft::new("send report").from_message("s1", "m7");
        d.due_at = Some(due);
        let r = f.capture(&me, d, Confirmation::user_confirmed()).unwrap();
        assert_eq!(r.source.session_id.as_deref(), Some("s1"));
        assert_eq!(r.source.message_id.as_deref(), Some("m7"));
        assert_eq!(r.due_at, Some(due));
        // Same owner from another session still sees it.
        assert_eq!(f.get(&me, r.id).unwrap().id, r.id);
        assert_eq!(f.list_open(&me, Utc::now()).len(), 1);
    }

    #[test]
    fn other_owner_and_non_tasks_are_isolated() {
        let mut f = FollowUps::new(InboxStore::in_memory());
        let (a, b) = (owner("a"), owner("b"));
        let r = f
            .capture(&a, Draft::new("x"), Confirmation::user_confirmed())
            .unwrap();
        assert!(f.list(&b, None).is_empty());
        assert!(matches!(f.get(&b, r.id), Err(InboxError::NotFound)));
        assert!(matches!(f.complete(&b, r.id), Err(InboxError::NotFound)));
        assert!(matches!(f.archive(&b, r.id), Err(InboxError::NotFound)));
        assert_eq!(f.get(&a, r.id).unwrap().state, State::Open);

        let note = f
            .store
            .create(&a, NewRecord::new(Kind::Note, "n", Provenance::User))
            .unwrap();
        assert!(matches!(f.complete(&a, note.id), Err(InboxError::NotFound)));
    }

    #[test]
    fn lifecycle_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let me = owner("me");
        let (keep, done, gone, later);
        {
            let mut f = FollowUps::new(InboxStore::open(dir.path()).unwrap());
            let c = Confirmation::user_confirmed;
            keep = f.capture(&me, Draft::new("keep"), c()).unwrap().id;
            done = f.capture(&me, Draft::new("done"), c()).unwrap().id;
            gone = f.capture(&me, Draft::new("gone"), c()).unwrap().id;
            later = f.capture(&me, Draft::new("later"), c()).unwrap().id;
            f.complete(&me, done).unwrap();
            f.archive(&me, gone).unwrap();
            let until = Utc::now() + Duration::hours(2);
            f.defer(&me, later, until).unwrap();
            assert!(f.defer(&me, keep, Utc::now() - Duration::hours(1)).is_err());
        }
        let mut f = FollowUps::new(InboxStore::open(dir.path()).unwrap());
        assert_eq!(f.get(&me, done).unwrap().state, State::Done);
        assert!(f.get(&me, done).unwrap().completed_at.is_some());
        assert_eq!(f.get(&me, gone).unwrap().state, State::Archived);
        assert!(f.list(&me, None).iter().all(|r| r.id != gone));
        let open = f.list_open(&me, Utc::now());
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, keep);
        let soon = Utc::now() + Duration::hours(3);
        assert_eq!(f.list_open(&me, soon).len(), 2);
        assert_eq!(f.reopen(&me, done).unwrap().state, State::Open);
    }
}
