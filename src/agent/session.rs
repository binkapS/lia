//! Session state and persistence.
//!
//! Sessions are stored as JSON documents under the workspace's `sessions/`
//! directory, one file per session, so a conversation survives a restart and
//! can be inspected by hand.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::provider::types::Message;

/// A single conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Short label shown in the session list.
    pub title: String,
    /// Where the session came from: `tui`, `cli`, `telegram`, ...
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    /// Arbitrary metadata set by the client that created the session.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl Session {
    pub fn new(id: impl Into<String>, source: impl Into<String>) -> Self {
        let now = Utc::now();
        Session {
            id: id.into(),
            title: "New session".into(),
            source: source.into(),
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Derive a title from the first user message.
    pub fn derive_title(&mut self) {
        if let Some(first) = self.messages.iter().find(|m| {
            matches!(
                m.content.first(),
                Some(crate::provider::types::ContentBlock::Text { .. })
            ) && m.role == crate::provider::types::Role::User
        }) {
            let text = first.text().trim().replace('\n', " ");
            if !text.is_empty() {
                let title: String = text.chars().take(60).collect();
                self.title = if text.chars().count() > 60 {
                    format!("{title}…")
                } else {
                    title
                };
            }
        }
    }

    /// Total characters across all messages — a rough size measure.
    pub fn size(&self) -> usize {
        self.messages
            .iter()
            .map(|m| {
                m.content
                    .iter()
                    .map(|b| match b {
                        crate::provider::types::ContentBlock::Text { text } => text.len(),
                        crate::provider::types::ContentBlock::Thinking { text } => text.len(),
                        crate::provider::types::ContentBlock::ToolUse { input, .. } => {
                            input.to_string().len()
                        }
                        crate::provider::types::ContentBlock::ToolResult { content, .. } => {
                            content.len()
                        }
                        crate::provider::types::ContentBlock::Image { .. } => 0,
                    })
                    .sum::<usize>()
            })
            .sum()
    }
}

/// Sessions, in memory, optionally backed by disk.
#[derive(Debug)]
pub struct SessionStore {
    sessions: HashMap<String, Session>,
    /// Where sessions are persisted; `None` keeps them in memory only.
    dir: Option<PathBuf>,
}

impl SessionStore {
    /// An in-memory store, for tests and ephemeral runs.
    pub fn in_memory() -> Self {
        SessionStore {
            sessions: HashMap::new(),
            dir: None,
        }
    }

    /// A store backed by `dir`, loading any sessions already there.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let mut store = SessionStore {
            sessions: HashMap::new(),
            dir: Some(dir),
        };
        store.load()?;
        Ok(store)
    }

    /// Read every session file in the store's directory.
    pub fn load(&mut self) -> std::io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(session) = serde_json::from_str::<Session>(&content) {
                self.sessions.insert(session.id.clone(), session);
            }
        }
        Ok(())
    }

    /// Write a session to disk, if the store has a directory.
    pub fn persist(&self, session: &Session) -> std::io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        write_session(dir, session)
    }

    /// The directory the store writes to, if any.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Write one session file to `dir` on the blocking thread pool.
    ///
    /// The write rewrites the whole history as one file, so it must not
    /// happen while the caller holds the sessions lock — everything else
    /// in the gateway reads through that same lock. The caller takes the
    /// lock only to make the snapshot and copy the directory out, then
    /// hands both here; the lock stays free while the disk works.
    pub async fn save_off_runtime(dir: Option<PathBuf>, session: Session) {
        let Some(dir) = dir else { return };
        let written = tokio::task::spawn_blocking(move || write_session(&dir, &session)).await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("could not save the session file: {e}"),
            Err(e) => tracing::warn!("the session save did not finish: {e}"),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.get_mut(id)
    }

    /// Fetch a session, creating it if it does not exist.
    pub fn get_or_create(&mut self, id: &str) -> Option<&mut Session> {
        if !self.sessions.contains_key(id) {
            let session = Session::new(id, "unknown");
            self.sessions.insert(id.to_string(), session);
        }
        self.sessions.get_mut(id)
    }

    pub fn insert(&mut self, session: Session) {
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn remove(&mut self, id: &str) -> Option<Session> {
        let session = self.sessions.remove(id);
        if let (Some(_session), Some(dir)) = (&session, &self.dir) {
            let _ = std::fs::remove_file(path_for(dir, id));
        }
        session
    }

    /// All sessions, newest first.
    pub fn list(&self) -> Vec<&Session> {
        let mut sessions: Vec<&Session> = self.sessions.values().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        sessions
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Where a session file lands, with any path-like part of the id removed.
///
/// Session ids come from the network, so strip anything path-like.
fn path_for(dir: &Path, id: &str) -> PathBuf {
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let safe = if safe.is_empty() {
        "session".to_string()
    } else {
        safe
    };
    dir.join(format!("{safe}.json"))
}

/// Write one session file: the work behind [`SessionStore::persist`].
///
/// A free function so the write can be handed to a blocking thread while
/// the store's lock stays free.
fn write_session(dir: &Path, session: &Session) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = path_for(dir, &session.id);
    let json = serde_json::to_string_pretty(session)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Write to a temp file and rename, so an interrupted write cannot
    // leave a half-written session behind.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
