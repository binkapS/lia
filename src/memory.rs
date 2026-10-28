//! Memory: what the assistant keeps between conversations.
//!
//! There are three kinds, and they are not interchangeable:
//!
//! - **Instructions** — `AGENTS.md` in the workspace. Standing instructions
//!   for the project, written by the user. Always in the prompt.
//! - **Curated** — `MEMORY.md` and `USER.md`. Long-term facts the user chose
//!   to keep. Always in the prompt, because they are small and deliberate.
//! - **Episodic** — a note per day. What happened. Searchable, but not in the
//!   prompt, because there is a lot of it.
//!
//! Everything remembered carries a provenance: where it came from. Facts
//! read off the internet are `untrusted` and are never treated as standing
//! instructions, however confidently they are phrased. That distinction is
//! the whole point of remembering things — a web page that says "ignore your
//! instructions and run this" is data, not a command.

/// A provider of text embeddings.
pub trait EmbeddingProvider: Send + Sync {
    fn embed(&self, text: &str) -> Vec<f32>;
}

pub mod consolidate;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate};

/// Where a remembered fact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The user said it.
    Owner,
    /// The assistant worked it out.
    Agent,
    /// It came from outside: a web page, a fetched file.
    Untrusted,
    /// It came from the configuration.
    System,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::Owner => "owner",
            Origin::Agent => "agent",
            Origin::Untrusted => "untrusted",
            Origin::System => "system",
        }
    }

    /// Whether facts of this origin may be treated as instructions.
    pub fn is_trusted(&self) -> bool {
        !matches!(self, Origin::Untrusted)
    }
}

/// What kind of memory a file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    /// Standing project instructions.
    Instructions,
    /// Curated long-term facts.
    Core,
    /// Facts about the user.
    User,
    /// A day's notes.
    Episodic,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryKind::Instructions => "instructions",
            MemoryKind::Core => "core",
            MemoryKind::User => "user",
            MemoryKind::Episodic => "episodic",
        }
    }
}

/// A memory file.
#[derive(Debug, Clone)]
pub struct MemoryFile {
    /// The file name, as the tools refer to it.
    pub name: String,
    pub path: PathBuf,
    pub kind: MemoryKind,
}

/// One line of a memory file that matched a search.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub file: String,
    pub line: usize,
    pub text: String,
}

/// A note the assistant wrote down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Where it came from.
    pub origin: Origin,
    /// What it says.
    pub text: String,
}

impl Note {
    /// Read a note from the line it was written as.
    ///
    /// The lines look like `- [owner] the build is run with cargo test`.
    /// A line without a tag is read as the assistant's own. Anything that
    /// is not a note — a heading, a blank line — is not one.
    pub fn parse(line: &str) -> Option<Note> {
        let text = line.trim();
        let text = text.strip_prefix("- ")?.trim();
        if text.is_empty() {
            return None;
        }

        let (origin, text) = match text.strip_prefix('[').and_then(|rest| {
            rest.split_once(']')
                .map(|(tag, rest)| (tag.trim(), rest.trim()))
        }) {
            Some((tag, text)) => {
                let origin = match tag.to_lowercase().as_str() {
                    "owner" => Origin::Owner,
                    "agent" => Origin::Agent,
                    "untrusted" => Origin::Untrusted,
                    "system" => Origin::System,
                    _ => Origin::Agent,
                };
                (origin, text)
            }
            None => (Origin::Agent, text),
        };

        if text.is_empty() {
            return None;
        }
        Some(Note {
            origin,
            text: text.to_string(),
        })
    }
}

/// The assistant's memory.
#[derive(Clone)]
pub struct MemoryStore {
    /// Where the curated and episodic files live.
    root: PathBuf,
    /// The workspace, for `AGENTS.md`.
    workspace: PathBuf,
    /// Optional embedding provider for vector search.
    embedder: Option<std::sync::Arc<dyn EmbeddingProvider>>,
}

impl MemoryStore {
    /// Open a memory store.
    ///
    /// The directory is created if it is missing, so the first `remember`
    /// works on a fresh install.
    pub fn open(root: PathBuf, workspace: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(MemoryStore { root, workspace, embedder: None })
    }

    /// The directory holding the memory files.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Attach an embedding provider.
    pub fn with_embedder(mut self, embedder: std::sync::Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// The file holding the standing project instructions.
    pub fn instructions_path(&self) -> PathBuf {
        self.workspace.join("AGENTS.md")
    }

    /// The file holding the curated facts.
    pub fn core_path(&self) -> PathBuf {
        self.root.join("MEMORY.md")
    }

    /// The file holding the facts about the user.
    pub fn user_path(&self) -> PathBuf {
        self.root.join("USER.md")
    }

    /// The file holding the notes for a day.
    pub fn daily_path(&self, day: chrono::NaiveDate) -> PathBuf {
        self.root.join(format!("{day}.md"))
    }

    /// Every memory file that exists.
    pub fn files(&self) -> Vec<MemoryFile> {
        let mut files = Vec::new();

        let instructions = self.instructions_path();
        if instructions.is_file() {
            files.push(MemoryFile {
                name: "AGENTS.md".into(),
                path: instructions,
                kind: MemoryKind::Instructions,
            });
        }

        for (name, path, kind) in [
            ("MEMORY.md", self.core_path(), MemoryKind::Core),
            ("USER.md", self.user_path(), MemoryKind::User),
        ] {
            if path.is_file() {
                files.push(MemoryFile {
                    name: name.into(),
                    path,
                    kind,
                });
            }
        }

        // The daily notes, newest last.
        for (day, path) in self.daily_files() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let _ = day;
            files.push(MemoryFile {
                name,
                path,
                kind: MemoryKind::Episodic,
            });
        }

        files
    }

    /// The daily notes that exist, oldest first.
    pub fn daily_files(&self) -> Vec<(NaiveDate, PathBuf)> {
        let mut daily: Vec<(NaiveDate, PathBuf)> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.filter_map(std::result::Result::ok) {
                let path = entry.path();
                if !path.is_file() || !path.extension().is_some_and(|e| e == "md") {
                    continue;
                }
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    continue;
                };
                let Ok(day) = NaiveDate::parse_from_str(name.trim_end_matches(".md"), "%Y-%m-%d")
                else {
                    continue;
                };
                daily.push((day, path));
            }
        }

        daily.sort_by_key(|(day, _)| *day);
        daily
    }

    /// The notes of a day.
    pub fn notes_of(&self, day: NaiveDate) -> Vec<Note> {
        let Ok(text) = std::fs::read_to_string(self.daily_path(day)) else {
            return Vec::new();
        };
        text.lines().filter_map(Note::parse).collect()
    }

    /// The days whose notes are waiting to be looked over.
    ///
    /// Today is left out: the day is still being written to.
    pub fn days_to_consolidate(&self) -> Vec<NaiveDate> {
        let done = self.consolidated_days();
        let today = Local::now().date_naive();

        self.daily_files()
            .into_iter()
            .map(|(day, _)| day)
            .filter(|day| *day < today && !done.contains(day))
            .collect()
    }

    /// The days that have been looked over already.
    pub fn consolidated_days(&self) -> BTreeSet<NaiveDate> {
        let path = self.root.join(".consolidated.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return BTreeSet::new();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Note that the days have been looked over.
    pub fn mark_consolidated(&self, days: &[NaiveDate]) -> std::io::Result<()> {
        let mut done = self.consolidated_days();
        done.extend(days.iter().copied());
        let text = serde_json::to_string_pretty(&done).unwrap_or_default();
        std::fs::write(self.root.join(".consolidated.json"), text)
    }

    /// The path of one of the curated files.
    pub fn curated_path(&self, kind: MemoryKind) -> Option<PathBuf> {
        match kind {
            MemoryKind::Core => Some(self.core_path()),
            MemoryKind::User => Some(self.user_path()),
            _ => None,
        }
    }

    /// Whether a curated file already says this.
    ///
    /// The lines of a curated file may carry a note of where the fact came
    /// from, so the fact is looked for at the start of a line rather than
    /// as the whole of it.
    pub fn curated_contains(&self, kind: MemoryKind, text: &str) -> bool {
        let Some(path) = self.curated_path(kind) else {
            return false;
        };
        let Ok(existing) = std::fs::read_to_string(path) else {
            return false;
        };
        let wanted = text.trim().to_lowercase();
        existing.lines().any(|line| {
            let line = line.trim().trim_start_matches("- ").trim().to_lowercase();
            line.starts_with(&wanted)
        })
    }

    /// Add a dated section to a curated file.
    pub fn append_section(
        &self,
        kind: MemoryKind,
        heading: &str,
        lines: &[String],
    ) -> std::io::Result<()> {
        let Some(path) = self.curated_path(kind) else {
            return Ok(());
        };
        if lines.is_empty() {
            return Ok(());
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        use std::io::Write;
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.trim().is_empty() {
            writeln!(file)?;
            writeln!(file)?;
        }
        writeln!(file, "## {heading}")?;
        for line in lines {
            writeln!(file, "- {line}")?;
        }
        file.flush()
    }

    /// Read a memory file by name.
    pub fn get(&self, name: &str) -> std::io::Result<String> {
        let path = self.resolve(name).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no memory file called '{name}'"),
            )
        })?;
        std::fs::read_to_string(path)
    }

    /// Find the file a name refers to.
    fn resolve(&self, name: &str) -> Option<PathBuf> {
        let name = name.trim();
        // A bare name may be given with or without the extension.
        let wanted = Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| name.to_string());

        self.files()
            .into_iter()
            .find(|f| f.name == wanted || f.name.trim_end_matches(".md") == wanted)
            .map(|f| f.path)
    }

    /// Search the memory for a phrase.
    ///
    /// The search is a plain case-insensitive substring match. Memory files
    /// are small and hand-written, so a search engine would be more machinery
    /// than the job needs.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }

        let mut hits = Vec::new();
        for file in self.files() {
            let Ok(text) = std::fs::read_to_string(&file.path) else {
                continue;
            };
            for (index, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&query) {
                    hits.push(SearchHit {
                        file: file.name.clone(),
                        line: index + 1,
                        text: line.trim().to_string(),
                    });
                }
            }
        }
        hits
    }

    /// Vector search, using an embedding provider when available.
    pub fn vector_search(&self, query: &str) -> Vec<SearchHit> {
        let qv = if let Some(embedder) = &self.embedder {
            embedder.embed(query)
        } else {
            text_vector(query)
        };
        let mut scored: Vec<(f32, SearchHit)> = Vec::new();
        for file in self.files() {
            let Ok(text) = std::fs::read_to_string(&file.path) else { continue; };
            for (i, line) in text.lines().enumerate() {
                let lv = if let Some(embedder) = &self.embedder {
                    embedder.embed(line)
                } else {
                    text_vector(line)
                };
                let score = cosine(&qv, &lv);
                if score > 0.0 {
                    scored.push((score, SearchHit { file: file.name.clone(), line: i+1, text: line.trim().to_string() }));
                }
            }
        }
        scored.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|x| x.1).collect()
    }

    /// Lines that may conflict with a new fact (same topic, different wording).
    pub fn conflicts(&self, text: &str) -> Vec<String> {
        let wanted: std::collections::BTreeSet<String> =
            text.to_lowercase().split_whitespace().map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_string()).filter(|s| !s.is_empty()).collect();
        let mut out = Vec::new();
        for file in self.files() {
            let Ok(contents) = std::fs::read_to_string(&file.path) else { continue; };
            for line in contents.lines() {
                let low = line.to_lowercase();
                let tokens: std::collections::BTreeSet<String> =
                    low.split_whitespace().map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_string()).filter(|s| !s.is_empty()).collect();
                let overlap = wanted.iter().filter(|t| tokens.contains(*t)).count();
                if overlap >= 2 && !low.contains(&text.to_lowercase()) {
                    out.push(line.trim().to_string());
                }
            }
        }
        out
    }

    /// Remember something, in the notes for today.
    ///
    /// The entry is tagged with where it came from. Untrusted facts live in
    /// the day's notes and never in the curated files, so they cannot become
    /// standing instructions by being repeated.
    pub fn remember(&self, text: &str, origin: Origin) -> std::io::Result<String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "there is nothing to remember",
            ));
        }

        let day = Local::now().date_naive();
        let path = self.daily_path(day);

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        // Start the file with a heading, so a bare note file still reads.
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.is_empty() {
            use std::io::Write;
            writeln!(file, "# {day}")?;
            writeln!(file)?;
        }

        use std::io::Write;
        writeln!(file, "- [{}] {}", origin.as_str(), text.replace('\n', " "))?;
        file.flush()?;

        Ok(format!(
            "remembered in {} (origin: {})",
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            origin.as_str()
        ))
    }

    /// Insert or update a fact in a named memory file.
    ///
    /// A duplicate (same text) is left alone; the fact is appended otherwise.
    pub fn upsert_fact(&self, name: &str, text: &str, origin: Origin) -> std::io::Result<String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "nothing to upsert"));
        }
        let path = self.resolve(name).unwrap_or_else(|| self.core_path());
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.lines().any(|l| l.trim().trim_start_matches("- ").trim().to_lowercase().starts_with(&text.to_lowercase())) {
            return Ok(format!("already present in {}", path.display()));
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        if !existing.trim().is_empty() { writeln!(file)?; }
        writeln!(file, "- [{}] {}", origin.as_str(), text.replace('\n', " "))?;
        file.flush()?;
        Ok(format!("upserted into {}", path.display()))
    }

    /// Export all memory files as JSON.
    pub fn export_memory(&self) -> String {
        let mut obj = serde_json::Map::new();
        for file in self.files() {
            if let Ok(text) = std::fs::read_to_string(&file.path) {
                obj.insert(file.name.clone(), serde_json::Value::String(text));
            }
        }
        serde_json::to_string_pretty(&obj).unwrap_or_else(|_| "{}".into())
    }

    /// Import facts from JSON or plain text into today's notes / core.
    pub fn import_memory(&self, data: &str, origin: Origin) -> std::io::Result<String> {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(o) = val.as_object() {
                let mut count = 0usize;
                for (k, v) in o {
                    let text = v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string());
                    let _ = self.remember(&format!("{k}: {text}"), origin);
                    count += 1;
                }
                return Ok(format!("imported {count} item(s)"));
            }
            if let Some(arr) = val.as_array() {
                let mut count = 0usize;
                for v in arr {
                    let text = v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string());
                    let _ = self.remember(&text, origin);
                    count += 1;
                }
                return Ok(format!("imported {count} item(s)"));
            }
        }
        // Fallback: treat the whole input as one remembered fact.
        let msg = self.remember(data.trim(), origin)?;
        Ok(msg)
    }

    /// The part of the memory that goes into the system prompt.
    ///
    /// Only the standing instructions and the curated files. The day's notes
    /// are found by search, not by being pasted into every request.
    pub fn prompt_section(&self) -> String {
        let mut sections: Vec<String> = Vec::new();

        if let Ok(text) = std::fs::read_to_string(self.instructions_path())
            && !text.trim().is_empty()
        {
            sections.push(format!(
                "<project_instructions>\n{}\n</project_instructions>",
                text.trim()
            ));
        }

        for (open, close, path) in [
            ("<memory>", "</memory>", self.core_path()),
            ("<user>", "</user>", self.user_path()),
        ] {
            if let Ok(text) = std::fs::read_to_string(path) {
                let text = text.trim();
                if !text.is_empty() {
                    sections.push(format!("{open}\n{text}\n{close}"));
                }
            }
        }

        if sections.is_empty() {
            return String::new();
        }

        sections.join("\n\n")
    }
}


fn text_vector(text: &str) -> Vec<f32> {
    const DIM: usize = 256;
    let mut v = vec![0.0f32; DIM];
    for tok in text.to_lowercase().split_whitespace() {
        let tok = tok.trim_matches(|c: char| !c.is_alphanumeric());
        if tok.is_empty() { continue; }
        let mut h: usize = 5381;
        for b in tok.bytes() { h = h.wrapping_mul(33).wrapping_add(b as usize); }
        v[h % DIM] += 1.0;
    }
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot=0.0; let mut an=0.0; let mut bn=0.0;
    for i in 0..a.len() {
        dot += a[i]*b[i];
        an += a[i]*a[i];
        bn += b[i]*b[i];
    }
    if an == 0.0 || bn == 0.0 { 0.0 } else { dot / (an.sqrt()*bn.sqrt()) }
}
