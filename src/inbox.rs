//! The user's own inbox: items, tasks, notes and reminders.
//!
//! Unlike the conversation-scoped list in [`crate::tool::todo`], records here
//! belong to an [`Owner`] and outlive any one session.
//!
//! Owner boundary: the store never derives an owner. Session and channel IDs
//! are not proof of identity, so the caller (gateway or CLI, once it has
//! authenticated someone) must supply the [`Owner`], never the model or a
//! message. Every operation is filtered by owner; another owner's record is
//! reported as not found.
//!
//! Several processes (the gateway and the CLI) may share one state directory.
//! Every mutation therefore takes an exclusive advisory lock on `inbox.lock`
//! (`File::lock`: `flock` on Unix, `LockFileEx` on Windows), re-reads the file
//! under that lock, applies the change and saves, so a concurrent writer's
//! changes are never overwritten. Reads re-read the file too; the atomic
//! rename on save means they never see a partial write and need no lock.
//! The lock is advisory and per open file: it assumes every writer uses this
//! type, and it is unreliable on some network file systems.
//!
//! Persistence is one versioned JSON file in the state directory, written
//! atomically. A file that is malformed or from a newer version makes
//! [`InboxStore::open`] fail and is left untouched, so nothing is lost or
//! overwritten silently.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const FILE_NAME: &str = "inbox.json";
const LOCK_NAME: &str = "inbox.lock";
/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// Why an inbox operation failed.
#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("inbox storage error: {0}")]
    Io(#[from] std::io::Error),
    #[error("inbox file {path} is not valid inbox data: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("inbox file {path} has schema version {found}, this build supports {SCHEMA_VERSION}")]
    UnsupportedVersion { path: PathBuf, found: u32 },
    #[error("inbox record not found")]
    NotFound,
    #[error("invalid inbox record: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, InboxError>;

/// An authenticated user identity, opaque to the store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Owner(String);

impl Owner {
    /// Wrap an identity established by the caller's authorization layer.
    pub fn new(id: impl Into<String>) -> Result<Owner> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(InboxError::Invalid("owner must not be empty"));
        }
        Ok(Owner(id))
    }
}

/// What a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Inbox,
    Task,
    Note,
    Reminder,
}

/// Where a record is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Done,
    Archived,
}

/// How much it matters; higher sorts first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
}

/// Who or what produced a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Typed or dictated by the user.
    User,
    /// Suggested by the assistant; not yet confirmed.
    Assistant,
    /// Derived from an external source.
    Imported,
}

/// Where a record was captured. Informational only, never identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub session_id: Option<String>,
    pub message_id: Option<String>,
    pub channel: Option<String>,
}

/// One stored record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub owner: Owner,
    pub kind: Kind,
    pub state: State,
    pub text: String,
    pub priority: Priority,
    pub due_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    /// When a due reminder was last pushed to the user.
    #[serde(default)]
    pub notified_at: Option<DateTime<Utc>>,
    pub source: Source,
    pub provenance: Provenance,
}

/// The fields needed to create a record.
#[derive(Debug, Clone)]
pub struct NewRecord {
    pub kind: Kind,
    pub text: String,
    pub priority: Priority,
    pub due_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub source: Source,
    pub provenance: Provenance,
}

impl NewRecord {
    /// A normal-priority record with no due time or source.
    pub fn new(kind: Kind, text: impl Into<String>, provenance: Provenance) -> NewRecord {
        NewRecord {
            kind,
            text: text.into(),
            priority: Priority::Normal,
            due_at: None,
            snoozed_until: None,
            source: Source::default(),
            provenance,
        }
    }
}

/// A partial update. `None` leaves a field alone; the nested `Option` on
/// `due_at` and `snoozed_until` sets (`Some(Some(t))`) or clears (`Some(None)`).
#[derive(Debug, Clone, Default)]
pub struct Update {
    pub kind: Option<Kind>,
    pub state: Option<State>,
    pub text: Option<String>,
    pub priority: Option<Priority>,
    pub due_at: Option<Option<DateTime<Utc>>>,
    pub snoozed_until: Option<Option<DateTime<Utc>>>,
    pub notified_at: Option<Option<DateTime<Utc>>>,
}

/// Which records to list.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub kind: Option<Kind>,
    pub state: Option<State>,
    /// Archived records are hidden unless asked for or `state` is `Archived`.
    pub include_archived: bool,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    records: Vec<Record>,
}

/// The durable store. Methods take `&mut self`; wrap in a lock to share.
#[derive(Debug, Default)]
pub struct InboxStore {
    path: Option<PathBuf>,
    records: Vec<Record>,
}

impl InboxStore {
    /// A store that is forgotten when dropped.
    pub fn in_memory() -> InboxStore {
        InboxStore::default()
    }

    /// Open (or start) the store in `dir`. Fails, leaving the file as is, if
    /// it cannot be read or understood.
    pub fn open(dir: impl AsRef<Path>) -> Result<InboxStore> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);

        let records = read_records(&path)?;
        Ok(InboxStore {
            path: Some(path),
            records,
        })
    }

    /// Create a record for `owner`.
    pub fn create(&mut self, owner: &Owner, new: NewRecord) -> Result<Record> {
        let text = clean_text(new.text)?;
        let now = Utc::now();
        let record = Record {
            id: Uuid::new_v4(),
            owner: owner.clone(),
            kind: new.kind,
            state: State::Open,
            text,
            priority: new.priority,
            due_at: new.due_at,
            snoozed_until: new.snoozed_until,
            created_at: now,
            updated_at: now,
            completed_at: None,
            archived_at: None,
            notified_at: None,
            source: new.source,
            provenance: new.provenance,
        };
        self.commit(|records| {
            records.push(record.clone());
            Ok(())
        })?;
        Ok(record)
    }

    /// One record of `owner`.
    pub fn get(&self, owner: &Owner, id: Uuid) -> Result<Record> {
        self.fresh()?
            .into_iter()
            .find(|r| r.id == id && &r.owner == owner)
            .ok_or(InboxError::NotFound)
    }

    /// The records of `owner`: open before done before archived, then higher
    /// priority, earlier due (undated last), older, and finally by ID.
    pub fn list(&self, owner: &Owner, filter: &Filter) -> Vec<Record> {
        // `list` cannot report an error; if the file cannot be read now, the
        // last records seen are shown rather than nothing.
        let records = self.fresh().unwrap_or_else(|e| {
            tracing::warn!("inbox could not be re-read, using the last copy: {e}");
            self.records.clone()
        });
        let mut found: Vec<Record> = records
            .iter()
            .filter(|r| &r.owner == owner)
            .filter(|r| filter.kind.is_none_or(|k| r.kind == k))
            .filter(|r| match filter.state {
                Some(s) => r.state == s,
                None => filter.include_archived || r.state != State::Archived,
            })
            .cloned()
            .collect();
        found.sort_by_key(|r| {
            (
                state_rank(r.state),
                Reverse(r.priority),
                r.due_at.is_none(),
                r.due_at,
                r.created_at,
                r.id,
            )
        });
        found
    }

    /// Apply `update`, maintaining the completion and archive timestamps.
    pub fn update(&mut self, owner: &Owner, id: Uuid, update: Update) -> Result<Record> {
        let text = update.text.clone().map(clean_text).transpose()?;
        self.commit(|records| {
            let index = records
                .iter()
                .position(|r| r.id == id && &r.owner == owner)
                .ok_or(InboxError::NotFound)?;
            let now = Utc::now();
            let mut record = records[index].clone();

            if let Some(kind) = update.kind {
                record.kind = kind;
            }
            if let Some(text) = text {
                record.text = text;
            }
            if let Some(priority) = update.priority {
                record.priority = priority;
            }
            if let Some(due) = update.due_at {
                if due != record.due_at {
                    record.notified_at = None;
                }
                record.due_at = due;
            }
            if let Some(notified) = update.notified_at {
                record.notified_at = notified;
            }
            if let Some(snooze) = update.snoozed_until {
                record.snoozed_until = snooze;
            }
            if let Some(state) = update.state
                && state != record.state
            {
                record.state = state;
                match state {
                    State::Open => {
                        record.completed_at = None;
                        record.archived_at = None;
                        record.notified_at = None;
                    }
                    State::Done => {
                        record.completed_at = Some(now);
                        record.archived_at = None;
                    }
                    // Archiving keeps an earlier completion time.
                    State::Archived => record.archived_at = Some(now),
                }
            }
            record.updated_at = now;

            records[index] = record.clone();
            Ok(record)
        })
    }

    /// Delete a record for good.
    pub fn delete(&mut self, owner: &Owner, id: Uuid) -> Result<()> {
        self.commit(|records| {
            let index = records
                .iter()
                .position(|r| r.id == id && &r.owner == owner)
                .ok_or(InboxError::NotFound)?;
            records.remove(index);
            Ok(())
        })
    }

    /// The records as the file holds them now.
    fn fresh(&self) -> Result<Vec<Record>> {
        match &self.path {
            Some(path) => read_records(path),
            None => Ok(self.records.clone()),
        }
    }

    /// Under the exclusive lock, reload from disk, apply `change` and persist.
    /// If `change` or the save fails, memory is left as the disk is.
    fn commit<T>(&mut self, change: impl FnOnce(&mut Vec<Record>) -> Result<T>) -> Result<T> {
        let _lock = match &self.path {
            Some(path) => {
                let lock = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(path.with_file_name(LOCK_NAME))?;
                lock.lock()?;
                self.records = read_records(path)?;
                Some(lock)
            }
            None => None,
        };
        let before = self.records.clone();
        let outcome = change(&mut self.records).and_then(|value| self.save().map(|()| value));
        if outcome.is_err() {
            self.records = before;
        }
        outcome
    }

    fn save(&self) -> Result<()> {
        use std::io::Write;
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = File {
            version: SCHEMA_VERSION,
            records: self.records.clone(),
        };
        let text = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        // Write beside the file and move it into place so an interrupted
        // write cannot leave a half-written inbox.
        let temporary = path.with_extension("json.tmp");
        let mut out = std::fs::File::create(&temporary)?;
        out.write_all(&text)?;
        out.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }
}

fn state_rank(state: State) -> u8 {
    match state {
        State::Open => 0,
        State::Done => 1,
        State::Archived => 2,
    }
}

fn clean_text(text: String) -> Result<String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(InboxError::Invalid("text must not be empty"));
    }
    Ok(text)
}

fn read_records(path: &Path) -> Result<Vec<Record>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

// Errors name the path and the parser's reason, never the record contents.
fn parse(path: &Path, text: &str) -> Result<Vec<Record>> {
    let malformed = |reason: String| InboxError::Malformed {
        path: path.to_path_buf(),
        reason,
    };
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| malformed(e.to_string()))?;
    let version = value
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| malformed("missing version".into()))?;
    if version != SCHEMA_VERSION as u64 {
        return Err(InboxError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: u32::try_from(version).unwrap_or(u32::MAX),
        });
    }
    let file: File = serde_json::from_value(value).map_err(|e| malformed(e.to_string()))?;
    Ok(file.records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(name: &str) -> Owner {
        Owner::new(name).unwrap()
    }

    fn note(text: &str) -> NewRecord {
        NewRecord::new(Kind::Note, text, Provenance::User)
    }

    #[test]
    fn records_survive_restart_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let alice = owner("alice");
        let mut store = InboxStore::open(dir.path()).unwrap();
        let mut new = NewRecord::new(Kind::Task, "buy milk", Provenance::Assistant);
        new.source = Source {
            session_id: Some("s1".into()),
            message_id: Some("m1".into()),
            channel: Some("telegram".into()),
        };
        let created = store.create(&alice, new).unwrap();
        drop(store);

        let store = InboxStore::open(dir.path()).unwrap();
        assert_eq!(store.get(&alice, created.id).unwrap(), created);
    }

    #[test]
    fn update_tracks_timestamps_and_delete_persists() {
        let dir = tempfile::tempdir().unwrap();
        let alice = owner("alice");
        let mut store = InboxStore::open(dir.path()).unwrap();
        let id = store.create(&alice, note("a")).unwrap().id;

        let done = store
            .update(
                &alice,
                id,
                Update {
                    state: Some(State::Done),
                    text: Some("  b ".into()),
                    ..Update::default()
                },
            )
            .unwrap();
        assert_eq!(done.text, "b");
        assert!(done.completed_at.is_some() && done.archived_at.is_none());

        let archived = store
            .update(
                &alice,
                id,
                Update {
                    state: Some(State::Archived),
                    ..Update::default()
                },
            )
            .unwrap();
        assert!(archived.completed_at.is_some() && archived.archived_at.is_some());

        let reopened = store
            .update(
                &alice,
                id,
                Update {
                    state: Some(State::Open),
                    ..Update::default()
                },
            )
            .unwrap();
        assert!(reopened.completed_at.is_none() && reopened.archived_at.is_none());

        assert!(
            store
                .update(
                    &alice,
                    id,
                    Update {
                        text: Some(" ".into()),
                        ..Update::default()
                    }
                )
                .is_err()
        );
        store.delete(&alice, id).unwrap();
        assert!(matches!(
            store.delete(&alice, id),
            Err(InboxError::NotFound)
        ));
        assert!(
            InboxStore::open(dir.path())
                .unwrap()
                .get(&alice, id)
                .is_err()
        );
    }

    #[test]
    fn owners_are_isolated() {
        let mut store = InboxStore::in_memory();
        let (alice, bob) = (owner("alice"), owner("bob"));
        let id = store.create(&alice, note("secret")).unwrap().id;
        assert!(store.list(&bob, &Filter::default()).is_empty());
        assert!(matches!(store.get(&bob, id), Err(InboxError::NotFound)));
        assert!(matches!(store.delete(&bob, id), Err(InboxError::NotFound)));
        assert!(store.update(&bob, id, Update::default()).is_err());
        assert_eq!(store.list(&alice, &Filter::default()).len(), 1);
    }

    #[test]
    fn listing_order_is_deterministic() {
        let mut store = InboxStore::in_memory();
        let a = owner("a");
        let mut low = note("low");
        low.priority = Priority::Low;
        let mut high = note("high");
        high.priority = Priority::High;
        let mut late = note("late");
        late.due_at = Some(Utc::now() + chrono::Duration::days(2));
        let mut soon = note("soon");
        soon.due_at = Some(Utc::now() + chrono::Duration::days(1));
        for n in [low, note("plain"), high, late, soon] {
            store.create(&a, n).unwrap();
        }
        let done = store.create(&a, note("done")).unwrap().id;
        store
            .update(
                &a,
                done,
                Update {
                    state: Some(State::Done),
                    ..Update::default()
                },
            )
            .unwrap();
        let arch = store.create(&a, note("arch")).unwrap().id;
        store
            .update(
                &a,
                arch,
                Update {
                    state: Some(State::Archived),
                    ..Update::default()
                },
            )
            .unwrap();

        let texts =
            |f: &Filter| -> Vec<String> { store.list(&a, f).into_iter().map(|r| r.text).collect() };
        assert_eq!(
            texts(&Filter::default()),
            ["high", "soon", "late", "plain", "low", "done"]
        );
        assert_eq!(texts(&Filter::default()), texts(&Filter::default()));
        assert_eq!(
            texts(&Filter {
                include_archived: true,
                ..Filter::default()
            })
            .last()
            .unwrap(),
            "arch"
        );
        assert_eq!(
            texts(&Filter {
                state: Some(State::Archived),
                ..Filter::default()
            }),
            ["arch"]
        );
    }

    #[test]
    fn malformed_or_newer_files_are_rejected_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let cases = [
            ("{ not json", false),
            (r#"{"records":[]}"#, false),
            (r#"{"version":1,"records":[{"id":"x"}]}"#, false),
            (r#"{"version":99,"records":[],"future":true}"#, true),
        ];
        for (content, unsupported) in cases {
            std::fs::write(&path, content).unwrap();
            let err = InboxStore::open(dir.path()).unwrap_err();
            assert_eq!(
                matches!(err, InboxError::UnsupportedVersion { found: 99, .. }),
                unsupported
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        }
    }

    #[test]
    fn a_second_store_sees_and_preserves_the_first_stores_writes() {
        let dir = tempfile::tempdir().unwrap();
        let me = owner("local");
        let mut gateway = InboxStore::open(dir.path()).unwrap();
        let mut cli = InboxStore::open(dir.path()).unwrap();

        let from_cli = cli.create(&me, note("from cli")).unwrap();
        // The long-lived store reads what the other process wrote...
        assert_eq!(gateway.list(&me, &Filter::default()), [from_cli.clone()]);
        assert_eq!(gateway.get(&me, from_cli.id).unwrap(), from_cli);

        // ...and a mutation of its own does not drop it.
        let from_gateway = gateway.create(&me, note("from gateway")).unwrap();
        assert_eq!(cli.list(&me, &Filter::default()).len(), 2);

        // Updating a stale-looking record works on the current file.
        gateway
            .update(
                &me,
                from_cli.id,
                Update {
                    state: Some(State::Done),
                    ..Update::default()
                },
            )
            .unwrap();
        cli.delete(&me, from_gateway.id).unwrap();
        gateway
            .update(&me, from_gateway.id, Update::default())
            .unwrap_err();

        let reopened = InboxStore::open(dir.path()).unwrap();
        let all = reopened.list(&me, &Filter::default());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, State::Done);
    }

    #[test]
    fn concurrent_creates_from_separate_stores_are_all_kept() {
        let dir = tempfile::tempdir().unwrap();
        let me = owner("local");
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let path = dir.path().to_path_buf();
                let me = me.clone();
                std::thread::spawn(move || {
                    let mut store = InboxStore::open(&path).unwrap();
                    for i in 0..10 {
                        store.create(&me, note(&format!("{t}-{i}"))).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let store = InboxStore::open(dir.path()).unwrap();
        assert_eq!(store.list(&me, &Filter::default()).len(), 40);
    }
}
