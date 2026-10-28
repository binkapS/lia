//! Pairing: letting a person in who has not been let in yet.
//!
//! A bot on a messaging platform can be reached by anyone. The safe
//! answer is an allow-list, but an allow-list has to be written before
//! the person arrives, and writing it needs the person to be known in
//! advance. Pairing is the other way round: the person says hello, the
//! bot answers with a code, and the owner approves that code. Until then
//! nothing they say reaches the assistant.
//!
//! A pairing is a grant for one person in one conversation. It is kept in
//! the state directory, so an approval survives a restart.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The letters and digits a code is made of.
///
/// The characters that are easy to confuse with one another are left out:
/// a code is read aloud and typed by hand.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// How long a request waits before the code is offered again.
const OFFER_AGAIN: chrono::Duration = chrono::Duration::minutes(10);

/// A person who has been let in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The code they were let in with.
    pub code: String,
    /// Which platform.
    pub channel: String,
    /// The conversation they talk in.
    pub conversation: String,
    /// Their id on that platform.
    pub user: String,
    /// Their name, when the platform gave one.
    #[serde(default)]
    pub name: Option<String>,
    /// When they were let in.
    pub granted_at: DateTime<Utc>,
}

impl Grant {
    /// How the grant is described to a person.
    pub fn describe(&self) -> String {
        let who = self.name.clone().unwrap_or_else(|| self.user.clone());
        format!(
            "{} — {} in {} (approved {})",
            self.code,
            who,
            self.conversation,
            self.granted_at.format("%Y-%m-%d %H:%M UTC")
        )
    }
}

/// A request to be let in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The code the owner has to approve.
    pub code: String,
    /// Which platform.
    pub channel: String,
    /// The conversation they talk in.
    pub conversation: String,
    /// Their id on that platform.
    pub user: String,
    /// Their name, when the platform gave one.
    #[serde(default)]
    pub name: Option<String>,
    /// When they asked.
    pub asked_at: DateTime<Utc>,
    /// When the code was last offered to them.
    #[serde(default)]
    pub announced_at: DateTime<Utc>,
}

impl Request {
    /// How the request is described to the owner.
    pub fn describe(&self) -> String {
        let who = self.name.clone().unwrap_or_else(|| self.user.clone());
        format!("{} — {} on {}", self.code, who, self.channel)
    }
}

/// What the store keeps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    /// The people who have been let in.
    #[serde(default)]
    grants: Vec<Grant>,
    /// The requests waiting to be approved.
    #[serde(default)]
    requests: Vec<Request>,
}

/// The store of who has been let in, and who is asking.
#[derive(Clone)]
pub struct PairingStore {
    /// Where the store is kept. `None` means it is only in memory.
    path: Option<PathBuf>,
    /// What the store holds.
    state: Arc<std::sync::Mutex<State>>,
}

impl PairingStore {
    /// Open a store, reading what is already there.
    ///
    /// A store that cannot be read is an error: quietly forgetting who was
    /// let in would lock them out.
    pub fn open(path: PathBuf) -> std::io::Result<PairingStore> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let state = match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("could not read {}: {e}", path.display()),
                )
            })?,
            _ => State::default(),
        };

        Ok(PairingStore {
            path: Some(path),
            state: Arc::new(std::sync::Mutex::new(state)),
        })
    }

    /// A store that is only in memory, for tests and for one-shot runs.
    pub fn in_memory() -> PairingStore {
        PairingStore {
            path: None,
            state: Arc::new(std::sync::Mutex::new(State::default())),
        }
    }

    /// Ask to be let in.
    ///
    /// The same person asking again in the same conversation keeps the
    /// same code, so the answer does not change under them. The second
    /// value says whether the code should be offered again: it is offered
    /// when the request is new, and after a while, but not for every
    /// message — otherwise a stranger could flood the channel.
    pub fn request(
        &self,
        channel: &str,
        conversation: &str,
        user: &str,
        name: Option<String>,
    ) -> (Request, bool) {
        let now = Utc::now();
        let mut state = self.state.lock().unwrap();

        if let Some(existing) = state.requests.iter_mut().find(|request| {
            request.channel == channel
                && request.conversation == conversation
                && request.user == user
        }) {
            let again = now - existing.announced_at > OFFER_AGAIN;
            if again {
                existing.announced_at = now;
            }
            if let Some(name) = name {
                existing.name = Some(name);
            }
            return (existing.clone(), again);
        }

        let request = Request {
            code: new_code(&mut state),
            channel: channel.to_string(),
            conversation: conversation.to_string(),
            user: user.to_string(),
            name,
            asked_at: now,
            announced_at: now,
        };
        state.requests.push(request.clone());
        self.save(&state);
        (request, true)
    }

    /// Let someone in, by the code they were given.
    pub fn approve(&self, code: &str) -> Result<Grant, String> {
        let wanted = normalize(code);
        let mut state = self.state.lock().unwrap();

        let Some(index) = state
            .requests
            .iter()
            .position(|request| normalize(&request.code) == wanted)
        else {
            return Err("that code is not one I know".into());
        };

        let request = state.requests.remove(index);
        let grant = Grant {
            code: request.code,
            channel: request.channel,
            conversation: request.conversation,
            user: request.user,
            name: request.name,
            granted_at: Utc::now(),
        };
        state.grants.push(grant.clone());
        self.save(&state);
        Ok(grant)
    }

    /// Refuse a request.
    pub fn deny(&self, code: &str) -> bool {
        let wanted = normalize(code);
        let mut state = self.state.lock().unwrap();
        let before = state.requests.len();
        state
            .requests
            .retain(|request| normalize(&request.code) != wanted);
        let denied = state.requests.len() != before;
        if denied {
            self.save(&state);
        }
        denied
    }

    /// Let someone out again, by the code they were let in with.
    pub fn revoke(&self, code: &str) -> bool {
        let wanted = normalize(code);
        let mut state = self.state.lock().unwrap();
        let before = state.grants.len();
        state
            .grants
            .retain(|grant| normalize(&grant.code) != wanted);
        let revoked = state.grants.len() != before;
        if revoked {
            self.save(&state);
        }
        revoked
    }

    /// Whether this person may talk in this conversation.
    pub fn is_granted(&self, channel: &str, conversation: &str, user: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.grants.iter().any(|grant| {
            grant.channel == channel && grant.conversation == conversation && grant.user == user
        })
    }

    /// The requests waiting to be approved.
    pub fn requests(&self) -> Vec<Request> {
        let state = self.state.lock().unwrap();
        state.requests.clone()
    }

    /// The people who have been let in.
    pub fn grants(&self) -> Vec<Grant> {
        let state = self.state.lock().unwrap();
        state.grants.clone()
    }

    /// The grant with this code, if there is one.
    pub fn grant(&self, code: &str) -> Option<Grant> {
        let wanted = normalize(code);
        let state = self.state.lock().unwrap();
        state
            .grants
            .iter()
            .find(|grant| normalize(&grant.code) == wanted)
            .cloned()
    }

    /// Write the store out.
    fn save(&self, state: &State) {
        let Some(path) = &self.path else {
            return;
        };
        match serde_json::to_string_pretty(state) {
            Ok(text) => {
                if let Err(e) = std::fs::write(path, text) {
                    tracing::warn!("could not write {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("could not write the pairing store: {e}"),
        }
    }
}

/// A code for a request, made so it cannot be guessed from another.
///
/// The randomness comes from the uuid the project already depends on.
fn new_code(state: &mut State) -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut code = String::new();
    for (index, byte) in bytes.iter().take(8).enumerate() {
        if index == 4 {
            code.push('-');
        }
        code.push(ALPHABET[(*byte as usize) % ALPHABET.len()] as char);
    }

    // A code is only unique within one store.
    let wanted = normalize(&code);
    let taken = state
        .requests
        .iter()
        .any(|entry| normalize(&entry.code) == wanted)
        || state
            .grants
            .iter()
            .any(|entry| normalize(&entry.code) == wanted);
    if taken {
        return new_code(state);
    }
    code
}

/// A code as it is compared, whatever the person typed.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// What a stranger is told when they are not let in yet.
pub fn pairing_message(code: &str) -> String {
    format!(
        "I'm lia, an assistant that runs on someone else's machine. I do not \
         talk to anyone until the owner says so.\n\n\
         Your pairing code is:\n\n    {code}\n\n\
         Send it to the owner, and this conversation will work once they approve it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_made_of_readable_characters() {
        let mut state = State::default();
        let code = new_code(&mut state);
        assert_eq!(code.len(), 9, "the code is {code}");
        assert!(code.contains('-'));
        for c in code.chars().filter(|c| *c != '-') {
            assert!(ALPHABET.contains(&(c as u8)), "{c} is not readable");
        }
    }

    #[test]
    fn asking_again_keeps_the_same_code() {
        let store = PairingStore::in_memory();
        let (first, offered) = store.request("telegram", "42", "42", Some("Ann".into()));
        assert!(offered, "the code was not offered");

        let (again, offered) = store.request("telegram", "42", "42", Some("Ann".into()));
        assert!(!offered, "the code was offered twice over");
        assert_eq!(first.code, again.code);
    }

    #[test]
    fn a_code_is_approved_and_the_person_is_let_in() {
        let store = PairingStore::in_memory();
        let (request, _) = store.request("telegram", "42", "42", Some("Ann".into()));

        let grant = store.approve(&request.code).expect("approved");
        assert_eq!(grant.user, "42");
        assert!(store.is_granted("telegram", "42", "42"));
        assert!(store.requests().is_empty(), "the request was left behind");
    }

    #[test]
    fn a_code_may_be_written_in_any_case() {
        let store = PairingStore::in_memory();
        let (request, _) = store.request("telegram", "42", "42", None);

        let grant = store
            .approve(&request.code.to_lowercase().replace('-', " "))
            .expect("approved");
        assert_eq!(grant.code, request.code);
    }

    #[test]
    fn a_code_that_is_not_known_is_refused() {
        let store = PairingStore::in_memory();
        assert!(store.approve("ZZZZ-9999").is_err());
    }

    #[test]
    fn a_grant_is_for_one_person_in_one_conversation() {
        let store = PairingStore::in_memory();
        let (request, _) = store.request("telegram", "42", "42", None);
        store.approve(&request.code).unwrap();

        assert!(!store.is_granted("telegram", "42", "43"), "another person");
        assert!(!store.is_granted("telegram", "43", "42"), "another chat");
        assert!(!store.is_granted("discord", "42", "42"), "another platform");
    }

    #[test]
    fn a_request_can_be_refused() {
        let store = PairingStore::in_memory();
        let (request, _) = store.request("telegram", "42", "42", None);

        assert!(store.deny(&request.code));
        assert!(store.requests().is_empty());
        assert!(!store.deny(&request.code), "it was refused twice");
    }

    #[test]
    fn a_grant_can_be_taken_back() {
        let store = PairingStore::in_memory();
        let (request, _) = store.request("telegram", "42", "42", None);
        store.approve(&request.code).unwrap();

        assert!(store.revoke(&request.code));
        assert!(!store.is_granted("telegram", "42", "42"));
    }

    #[test]
    fn the_store_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("lia-pairing-{}", uuid::Uuid::new_v4()));
        let path = dir.join("pairing.json");

        let store = PairingStore::open(path.clone()).unwrap();
        let (request, _) = store.request("telegram", "42", "42", Some("Ann".into()));
        store.approve(&request.code).unwrap();

        let reopened = PairingStore::open(path).unwrap();
        assert!(reopened.is_granted("telegram", "42", "42"));
        assert_eq!(reopened.grants().len(), 1);

        let _ = std::fs::remove_dir_all(dir);
    }
}
