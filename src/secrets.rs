//! Secrets kept outside the environment.
//!
//! API keys and bot tokens normally come from the environment. `lia setup`
//! writes them to a file with owner-only permissions instead, so they need
//! not be exported in every shell.
//!
//! The real environment always wins: a variable that is exported is used
//! before the stored one. That keeps existing setups working, and makes it
//! possible to override a stored key for one command without editing the
//! file.
//!
//! The file is a flat TOML table of environment variable names:
//!
//! ```toml
//! OPENAI_API_KEY = "..."
//! TELEGRAM_BOT_TOKEN = "..."
//! ```
//!
//! It is written with mode `0600` on Unix, and a directory that is created
//! for it is made `0700`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The name of the file beside the configuration.
pub const FILE_NAME: &str = "secrets.toml";

/// A table of secrets, keyed by the environment variable they stand for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secrets {
    entries: BTreeMap<String, String>,
}

impl Secrets {
    /// An empty table.
    pub fn new() -> Self {
        Secrets::default()
    }

    /// Read the table from a file.
    ///
    /// A file that is not there is an empty table rather than an error: it
    /// only means `lia setup` has not been run yet.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Secrets::new());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Secrets::new());
        }
        toml::from_str(&text).with_context(|| format!("could not parse {}", path.display()))
    }

    /// Write the table to a file, owner-readable only.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            let existed = parent.exists();
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
            // A directory that was already there belongs to the user, and
            // its permissions are theirs to choose. Only one that was made
            // for the secrets is restricted.
            if !existed {
                restrict_directory(parent)?;
            }
        }

        let text = toml::to_string_pretty(self).context("could not write the secrets as TOML")?;

        // Write beside the file and rename, so a crash cannot leave half a
        // table behind.
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(FILE_NAME);
        let temporary = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            write_private(&temporary, text.as_bytes())?;
            std::fs::rename(&temporary, path)
                .with_context(|| format!("could not put the secrets at {}", path.display()))?;
            restrict_file(path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    /// The value stored under a name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(String::as_str)
    }

    /// Store a value, trimmed. An empty value removes the entry.
    pub fn set(&mut self, name: &str, value: &str) {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            return;
        }
        if value.is_empty() {
            self.entries.remove(name);
        } else {
            self.entries.insert(name.to_string(), value.to_string());
        }
    }

    /// Remove a name.
    pub fn remove(&mut self, name: &str) -> bool {
        self.entries.remove(name.trim()).is_some()
    }

    /// Whether the table has an entry for a name.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// The names in the table, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// How many secrets are stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A description of a stored value that does not give it away.
    pub fn describe(&self, name: &str) -> String {
        let Some(value) = self.entries.get(name) else {
            return "not set".to_string();
        };
        describe_value(value)
    }
}

/// Describe a secret without printing it.
pub fn describe_value(value: &str) -> String {
    let characters: Vec<char> = value.trim().chars().collect();
    if characters.is_empty() {
        return "empty".to_string();
    }
    if characters.len() <= 8 {
        return format!("set ({} character(s))", characters.len());
    }
    let tail: String = characters[characters.len() - 4..].iter().collect();
    format!("set (…{tail})")
}

/// Where the secrets file lives.
///
/// A configuration file of its own keeps its secrets beside it. Without
/// one, the file sits with the default configuration. `LIA_SECRETS_FILE`
/// overrides both, which is useful for tests and for an installation that
/// keeps its configuration somewhere else.
pub fn path_for(config_path: Option<&Path>) -> PathBuf {
    if let Some(path) = std::env::var_os("LIA_SECRETS_FILE") {
        return PathBuf::from(path);
    }
    match config_path {
        Some(path) => path
            .parent()
            .map(|parent| parent.join(FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(FILE_NAME)),
        None => dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lia")
            .join(FILE_NAME),
    }
}

static STORE: OnceLock<Secrets> = OnceLock::new();

/// Load the stored secrets once, before anything asks for a key.
///
/// A missing file is not an error. A file that cannot be read is left out
/// with a warning rather than stopping the assistant: the environment may
/// still have everything that is needed.
pub fn init(path: &Path) -> Result<()> {
    if STORE.get().is_some() {
        return Ok(());
    }
    match Secrets::load(path) {
        Ok(secrets) => {
            let _ = STORE.set(secrets);
            Ok(())
        }
        Err(e) => {
            tracing::warn!("the stored secrets were not read: {e}");
            Ok(())
        }
    }
}

/// The value of an environment variable, or the stored one.
///
/// An exported variable wins over the file, and an empty variable counts as
/// unset.
pub fn var(name: &str) -> Option<String> {
    real_env(name).or_else(|| {
        STORE
            .get()
            .and_then(|secrets| secrets.get(name))
            .map(str::to_string)
    })
}

/// The value of an environment variable, ignoring the stored secrets.
pub fn real_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Make the directory private to its owner.
#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not restrict {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<()> {
    Ok(())
}

/// Make the file private to its owner.
#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

/// Write a file that only its owner may read.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("could not write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("could not save {}", path.display()))?;
    restrict_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lia-secrets-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn a_secret_is_written_and_read_back() {
        let dir = scratch("round-trip");
        let path = dir.join(FILE_NAME);
        let mut secrets = Secrets::new();
        secrets.set("OPENAI_API_KEY", "  sk-test  ");
        secrets.set("TELEGRAM_BOT_TOKEN", "123:abc");
        // An empty value removes the entry.
        secrets.set("DISCORD_BOT_TOKEN", "");
        secrets.save(&path).expect("the file is written");

        let read = Secrets::load(&path).expect("the file is read");
        assert_eq!(read.get("OPENAI_API_KEY"), Some("sk-test"));
        assert_eq!(read.get("TELEGRAM_BOT_TOKEN"), Some("123:abc"));
        assert!(!read.contains("DISCORD_BOT_TOKEN"));
        assert_eq!(read.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("permissions");
        let path = dir.join(FILE_NAME);
        let mut secrets = Secrets::new();
        secrets.set("OPENAI_API_KEY", "sk-test");
        secrets.save(&path).expect("the file is written");

        let mode = std::fs::metadata(&path)
            .expect("the file is there")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the secrets are readable by others: {mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_secret_is_described_without_being_given_away() {
        let mut secrets = Secrets::new();
        secrets.set("OPENAI_API_KEY", "sk-1234567890abcdef");

        let described = secrets.describe("OPENAI_API_KEY");
        assert!(described.contains("cdef"), "{described}");
        assert!(!described.contains("1234567890"), "{described}");
        assert_eq!(secrets.describe("ANTHROPIC_API_KEY"), "not set");
    }

    #[test]
    fn a_file_that_is_not_there_is_empty() {
        let path = scratch("missing").join(FILE_NAME);
        let secrets = Secrets::load(&path).expect("a missing file is fine");
        assert!(secrets.is_empty());
    }
}
