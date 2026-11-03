//! Opt-in daily routines that deliver a deterministic briefing.
//!
//! A routine exists only after it is explicitly created, and names exactly
//! one chat target. Only the routine definition and its run status are
//! persisted (`routines.json`); the generated briefing text never is.
//!
//! Delivery semantics (at-least-once): a routine's next run is advanced and
//! saved only after the delivery attempt has finished, so a crash between
//! sending and saving can repeat a briefing but never silently drop one.
//!
//! - A failed delivery (including an unregistered channel) is retried every
//!   [`RETRY_DELAY`] up to [`MAX_ATTEMPTS`] attempts in total.
//! - After a restart, a slot missed by no more than [`CATCH_UP_WINDOW`] is
//!   delivered once; older slots are recorded as missed. Missed slots are
//!   never replayed individually.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::briefing::{BriefingOptions, BriefingSources, collect_briefing};
use crate::channel::{Channel, OutboundMessage};
use crate::inbox::{InboxStore, Owner};
use crate::schedule::{JobStore, Scheduler, ZonedSchedule};

/// Delay before a failed delivery is retried.
pub const RETRY_DELAY: Duration = Duration::minutes(5);
/// Total delivery attempts per slot.
pub const MAX_ATTEMPTS: u32 = 3;
/// How late a slot may still be delivered.
pub const CATCH_UP_WINDOW: Duration = Duration::hours(2);

/// Looks up a delivery channel by name.
pub type ChannelLookup<'a> = &'a (dyn Fn(&str) -> Option<Arc<dyn Channel>> + Sync);

/// The single chat a routine delivers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineTarget {
    pub channel: String,
    pub conversation: String,
}

/// What happened the last time a routine ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RunOutcome {
    Delivered,
    /// Delivery failed; a retry is scheduled.
    Retrying {
        error: String,
    },
    /// Delivery failed for good.
    Failed {
        error: String,
    },
    /// The slot was too old to deliver after a restart or long pause.
    Missed,
}

/// A persisted routine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    /// Identity whose inbox the briefing reads.
    pub owner: String,
    pub schedule: ZonedSchedule,
    pub due_items: bool,
    pub upcoming_jobs: bool,
    pub approved_notes: bool,
    pub max_bytes: usize,
    pub target: RoutineTarget,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_outcome: Option<RunOutcome>,
    /// When the next attempt happens; `None` once nothing is left to run.
    pub next_run: Option<DateTime<Utc>>,
    /// The scheduled slot currently being attempted.
    slot: Option<DateTime<Utc>>,
    attempts: u32,
}

/// What a person needs to see about a routine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineStatus {
    pub id: String,
    pub enabled: bool,
    pub schedule: String,
    pub timezone: String,
    pub target: RoutineTarget,
    pub last_run: Option<DateTime<Utc>>,
    pub last_outcome: Option<RunOutcome>,
    pub next_run: Option<DateTime<Utc>>,
}

/// What to create.
#[derive(Debug, Clone)]
pub struct NewRoutine {
    pub id: String,
    pub owner: String,
    pub schedule: ZonedSchedule,
    pub sources: BriefingSources,
    pub max_bytes: usize,
    pub target: RoutineTarget,
}

/// The routines, kept on disk.
pub struct RoutineStore {
    path: Option<PathBuf>,
    routines: Vec<Routine>,
}

impl RoutineStore {
    pub fn in_memory() -> RoutineStore {
        RoutineStore {
            path: None,
            routines: Vec::new(),
        }
    }

    /// Open `routines.json` in `dir`. An unreadable file is an error rather
    /// than silently forgetting routines.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<RoutineStore> {
        let path = dir.as_ref().join("routines.json");
        let routines = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        } else {
            Vec::new()
        };
        Ok(RoutineStore {
            path: Some(path),
            routines,
        })
    }

    pub fn list(&self) -> &[Routine] {
        &self.routines
    }

    pub fn get(&self, id: &str) -> Option<&Routine> {
        self.routines.iter().find(|r| r.id == id)
    }

    pub fn status(&self, id: &str) -> Option<RoutineStatus> {
        self.get(id).map(status_of)
    }

    pub fn statuses(&self) -> Vec<RoutineStatus> {
        self.routines.iter().map(status_of).collect()
    }

    /// Opt in to a routine. Its first run is the next scheduled slot.
    pub fn create(&mut self, new: NewRoutine, now: DateTime<Utc>) -> Result<RoutineStatus, String> {
        if new.id.trim().is_empty() || new.owner.trim().is_empty() {
            return Err("a routine needs an id and an owner".into());
        }
        if new.target.channel.trim().is_empty() || new.target.conversation.trim().is_empty() {
            return Err("a routine needs an explicit channel and conversation".into());
        }
        if new.max_bytes == 0 {
            return Err("max_bytes must be greater than zero".into());
        }
        if !(new.sources.due_items || new.sources.upcoming_jobs || new.sources.approved_notes) {
            return Err("select at least one briefing source".into());
        }
        if self.get(&new.id).is_some() {
            return Err(format!("routine \"{}\" already exists", new.id));
        }
        let next_run = new
            .schedule
            .next_after(now)
            .ok_or("the schedule has no upcoming run")?;
        let routine = Routine {
            id: new.id,
            owner: new.owner,
            schedule: new.schedule,
            due_items: new.sources.due_items,
            upcoming_jobs: new.sources.upcoming_jobs,
            approved_notes: new.sources.approved_notes,
            max_bytes: new.max_bytes,
            target: new.target,
            enabled: true,
            created_at: now,
            last_run: None,
            last_outcome: None,
            next_run: Some(next_run),
            slot: Some(next_run),
            attempts: 0,
        };
        let status = status_of(&routine);
        self.routines.push(routine);
        self.save().map_err(|e| e.to_string())?;
        Ok(status)
    }

    /// Stop a routine without forgetting it.
    pub fn pause(&mut self, id: &str) -> Result<RoutineStatus, String> {
        let routine = self.find_mut(id)?;
        routine.enabled = false;
        routine.attempts = 0;
        let status = status_of(routine);
        self.save().map_err(|e| e.to_string())?;
        Ok(status)
    }

    /// Resume from the next slot after `now`; paused time is not caught up.
    pub fn resume(&mut self, id: &str, now: DateTime<Utc>) -> Result<RoutineStatus, String> {
        let routine = self.find_mut(id)?;
        let next = routine
            .schedule
            .next_after(now)
            .ok_or("the schedule has no upcoming run")?;
        routine.enabled = true;
        routine.next_run = Some(next);
        routine.slot = Some(next);
        routine.attempts = 0;
        let status = status_of(routine);
        self.save().map_err(|e| e.to_string())?;
        Ok(status)
    }

    pub fn remove(&mut self, id: &str) -> std::io::Result<bool> {
        let before = self.routines.len();
        self.routines.retain(|r| r.id != id);
        let removed = self.routines.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Run what is due at `now`, delivering each briefing through the
    /// channel named by its target. `jobs` feeds the upcoming-jobs source.
    pub async fn tick(
        &mut self,
        now: DateTime<Utc>,
        inbox: Option<&InboxStore>,
        jobs: Option<&JobStore>,
        channel: ChannelLookup<'_>,
    ) -> Vec<RoutineStatus> {
        let mut ran = Vec::new();
        for index in 0..self.routines.len() {
            let routine = &self.routines[index];
            let (Some(next_run), Some(slot)) = (routine.next_run, routine.slot) else {
                continue;
            };
            if !routine.enabled || next_run > now {
                continue;
            }

            let result = if now - slot > CATCH_UP_WINDOW {
                None
            } else {
                Some(deliver(routine, now, inbox, jobs, channel).await)
            };

            let routine = &mut self.routines[index];
            routine.last_run = Some(now);
            let advance = match result {
                None => {
                    routine.last_outcome = Some(RunOutcome::Missed);
                    true
                }
                Some(Ok(())) => {
                    routine.last_outcome = Some(RunOutcome::Delivered);
                    true
                }
                Some(Err(error)) => {
                    routine.attempts += 1;
                    let retry_at = now + RETRY_DELAY;
                    if routine.attempts < MAX_ATTEMPTS && retry_at - slot <= CATCH_UP_WINDOW {
                        routine.last_outcome = Some(RunOutcome::Retrying { error });
                        routine.next_run = Some(retry_at);
                        false
                    } else {
                        routine.last_outcome = Some(RunOutcome::Failed { error });
                        true
                    }
                }
            };
            if advance {
                routine.attempts = 0;
                routine.next_run = routine.schedule.next_after(now);
                routine.slot = routine.next_run;
                if routine.next_run.is_none() {
                    routine.enabled = false;
                }
            }
            ran.push(status_of(routine));
        }
        if !ran.is_empty() {
            if let Err(e) = self.save() {
                tracing::warn!("could not save the routines: {e}");
            }
        }
        ran
    }

    /// [`tick`](Self::tick) using a [`Scheduler`]'s channels and jobs.
    pub async fn tick_with_scheduler(
        &mut self,
        now: DateTime<Utc>,
        inbox: Option<&InboxStore>,
        scheduler: &Scheduler,
    ) -> Vec<RoutineStatus> {
        let mut jobs = JobStore::in_memory();
        for job in scheduler.list().await {
            jobs.add(job);
        }
        self.tick(now, inbox, Some(&jobs), &|name| scheduler.channel(name))
            .await
    }

    fn find_mut(&mut self, id: &str) -> Result<&mut Routine, String> {
        self.routines
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no routine \"{id}\""))
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.routines)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }
}

async fn deliver(
    routine: &Routine,
    now: DateTime<Utc>,
    inbox: Option<&InboxStore>,
    jobs: Option<&JobStore>,
    channel: ChannelLookup<'_>,
) -> Result<(), String> {
    let owner = Owner::new(routine.owner.clone()).map_err(|e| e.to_string())?;
    let adapter = channel(&routine.target.channel)
        .ok_or_else(|| format!("channel \"{}\" is not available", routine.target.channel))?;
    let options = BriefingOptions {
        sources: BriefingSources {
            due_items: routine.due_items,
            upcoming_jobs: routine.upcoming_jobs,
            approved_notes: routine.approved_notes,
        },
        max_bytes: routine.max_bytes,
        ..BriefingOptions::default()
    };
    let text = collect_briefing(&owner, inbox, jobs, now, &options);
    adapter
        .send(&OutboundMessage::new(
            routine.target.conversation.clone(),
            text,
        ))
        .await
        .map_err(|e| e.to_string())
}

fn status_of(routine: &Routine) -> RoutineStatus {
    RoutineStatus {
        id: routine.id.clone(),
        enabled: routine.enabled,
        schedule: routine.schedule.when().describe(),
        timezone: routine.schedule.timezone().to_string(),
        target: routine.target.clone(),
        last_run: routine.last_run,
        last_outcome: routine.last_outcome.clone(),
        next_run: routine.next_run,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::TimeZone;

    use super::*;
    use crate::channel::{InboundSender, OutboundSender};
    use crate::schedule::When;

    struct Recorder {
        sent: Mutex<Vec<OutboundMessage>>,
        fail: bool,
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
            if self.fail {
                anyhow::bail!("down");
            }
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }
    }

    fn recorder(fail: bool) -> Arc<Recorder> {
        Arc::new(Recorder {
            sent: Mutex::new(Vec::new()),
            fail,
        })
    }

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 15, h, m, 0).unwrap()
    }

    fn new_routine() -> NewRoutine {
        NewRoutine {
            id: "morning".into(),
            owner: "me".into(),
            // 08:00 in Berlin is 07:00 UTC in January.
            schedule: ZonedSchedule::new(When::Daily { hour: 8, minute: 0 }, "Europe/Berlin")
                .unwrap(),
            sources: BriefingSources::default(),
            max_bytes: 1024,
            target: RoutineTarget {
                channel: "test".into(),
                conversation: "chat-1".into(),
            },
        }
    }

    fn lookup(r: &Arc<Recorder>) -> impl Fn(&str) -> Option<Arc<dyn Channel>> + Sync + use<> {
        let r = r.clone();
        move |name| (name == "test").then(|| r.clone() as Arc<dyn Channel>)
    }

    #[tokio::test]
    async fn delivers_at_local_time_to_one_target_and_advances() {
        let mut store = RoutineStore::in_memory();
        let status = store.create(new_routine(), at(5, 0)).unwrap();
        assert_eq!(status.next_run, Some(at(7, 0)));

        let rec = recorder(false);
        let find = lookup(&rec);
        assert!(store.tick(at(6, 59), None, None, &find).await.is_empty());
        let ran = store.tick(at(7, 0), None, None, &find).await;
        assert_eq!(ran[0].last_outcome, Some(RunOutcome::Delivered));
        assert_eq!(
            ran[0].next_run,
            Some(Utc.with_ymd_and_hms(2026, 1, 16, 7, 0, 0).unwrap())
        );
        let sent = rec.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].conversation, "chat-1");
    }

    #[tokio::test]
    async fn retries_then_gives_up_and_paused_routines_do_not_run() {
        let mut store = RoutineStore::in_memory();
        store.create(new_routine(), at(5, 0)).unwrap();
        let rec = recorder(true);
        let find = lookup(&rec);

        let ran = store.tick(at(7, 0), None, None, &find).await;
        assert!(matches!(
            ran[0].last_outcome,
            Some(RunOutcome::Retrying { .. })
        ));
        assert_eq!(ran[0].next_run, Some(at(7, 5)));
        store.tick(at(7, 5), None, None, &find).await;
        let ran = store.tick(at(7, 10), None, None, &find).await;
        assert!(matches!(
            ran[0].last_outcome,
            Some(RunOutcome::Failed { .. })
        ));
        assert!(ran[0].next_run.unwrap() > at(7, 10));

        store.pause("morning").unwrap();
        let much_later = at(12, 0) + Duration::days(1);
        assert!(store.tick(much_later, None, None, &find).await.is_empty());
    }

    #[tokio::test]
    async fn restart_catches_up_once_within_window_otherwise_misses() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RoutineStore::open(dir.path()).unwrap();
        store.create(new_routine(), at(5, 0)).unwrap();
        drop(store);

        let mut store = RoutineStore::open(dir.path()).unwrap();
        let rec = recorder(false);
        let find = lookup(&rec);
        let ran = store.tick(at(8, 30), None, None, &find).await;
        assert_eq!(ran[0].last_outcome, Some(RunOutcome::Delivered));
        assert_eq!(rec.sent.lock().unwrap().len(), 1);

        let later = at(7, 0) + Duration::days(3) + Duration::hours(5);
        let ran = store.tick(later, None, None, &find).await;
        assert_eq!(ran[0].last_outcome, Some(RunOutcome::Missed));
        assert_eq!(rec.sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn creation_requires_explicit_target_and_a_source() {
        let mut store = RoutineStore::in_memory();
        let mut bad = new_routine();
        bad.target.conversation = " ".into();
        assert!(store.create(bad, at(5, 0)).is_err());
        let mut bad = new_routine();
        bad.sources = BriefingSources {
            due_items: false,
            upcoming_jobs: false,
            approved_notes: false,
        };
        assert!(store.create(bad, at(5, 0)).is_err());
        assert!(store.list().is_empty());
    }
}
