//! Approval-bound preview/confirmation boundary for external actions (F12).
//!
//! This module only decides whether a user's approval covers an exact action.
//! It performs no external write: callers must call [`ExternalActionGate::confirm`]
//! and only act on `Ok`. Standing tool approvals or connector consent never
//! satisfy this gate; every action needs its own preview and confirmation.
//!
//! Action content is exposed only through [`ActionPreview`] fields meant for
//! the user. `Debug`, `Display` and error types never contain it.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// A fully specified external action, exactly as it would be executed.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalAction {
    pub owner: String,
    pub account: String,
    pub target: String,
    pub payload: String,
    pub side_effects: String,
}

impl ExternalAction {
    /// SHA-256 (hex) over the length-prefixed owner, account, target and payload.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        for part in [&self.owner, &self.account, &self.target, &self.payload] {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
        hex::encode(h.finalize())
    }
}

impl fmt::Debug for ExternalAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalAction")
            .field("digest", &self.digest())
            .finish_non_exhaustive()
    }
}

/// What the user reviews before approving. Show it; do not log it.
#[derive(Clone)]
pub struct ActionPreview {
    pub id: String,
    pub account: String,
    pub target: String,
    pub payload: String,
    pub side_effects: String,
    pub digest: String,
    pub expires_at: SystemTime,
}

impl fmt::Debug for ActionPreview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActionPreview")
            .field("id", &self.id)
            .field("digest", &self.digest)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Why an action was not authorised. Carries no action content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    Noninteractive,
    Unknown,
    WrongOwner,
    Expired,
    PayloadChanged,
}

impl fmt::Display for Denial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Noninteractive => "external actions are denied in noninteractive mode",
            Self::Unknown => "no pending approval (unknown or already used)",
            Self::WrongOwner => "approval belongs to a different owner",
            Self::Expired => "approval expired",
            Self::PayloadChanged => "action changed since it was previewed",
        })
    }
}

impl std::error::Error for Denial {}

/// Proof that the exact action was approved. Returned once per approval.
#[derive(Debug, PartialEq, Eq)]
pub struct Approved {
    pub digest: String,
}

struct Pending {
    owner: String,
    digest: String,
    expires_at: SystemTime,
}

/// Issues previews and consumes one-time confirmations.
pub struct ExternalActionGate {
    ttl: Duration,
    pending: Mutex<HashMap<String, Pending>>,
}

impl ExternalActionGate {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Registers the action and returns the preview to show the user.
    /// Denied outright when `interactive` is false.
    pub fn preview(
        &self,
        action: &ExternalAction,
        interactive: bool,
        now: SystemTime,
    ) -> Result<ActionPreview, Denial> {
        if !interactive {
            return Err(Denial::Noninteractive);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let digest = action.digest();
        let expires_at = now + self.ttl;
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, p| p.expires_at > now);
        pending.insert(
            id.clone(),
            Pending {
                owner: action.owner.clone(),
                digest: digest.clone(),
                expires_at,
            },
        );
        Ok(ActionPreview {
            id,
            account: action.account.clone(),
            target: action.target.clone(),
            payload: action.payload.clone(),
            side_effects: action.side_effects.clone(),
            digest,
            expires_at,
        })
    }

    /// Records the user's approval for `id`, valid only for this exact action.
    /// The approval is consumed on any attempt that reaches the owner check,
    /// so a stale or tampered action cannot be retried against it.
    pub fn confirm(
        &self,
        id: &str,
        action: &ExternalAction,
        interactive: bool,
        now: SystemTime,
    ) -> Result<Approved, Denial> {
        if !interactive {
            return Err(Denial::Noninteractive);
        }
        let mut pending = self.pending.lock().unwrap();
        let entry = pending.get(id).ok_or(Denial::Unknown)?;
        if entry.owner != action.owner {
            return Err(Denial::WrongOwner);
        }
        let entry = pending.remove(id).ok_or(Denial::Unknown)?;
        if entry.expires_at <= now {
            return Err(Denial::Expired);
        }
        let digest = action.digest();
        if digest != entry.digest {
            return Err(Denial::PayloadChanged);
        }
        Ok(Approved { digest })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action() -> ExternalAction {
        ExternalAction {
            owner: "alice".into(),
            account: "work".into(),
            target: "bob@example.com".into(),
            payload: "secret body".into(),
            side_effects: "sends an email".into(),
        }
    }

    fn gate() -> (ExternalActionGate, SystemTime) {
        (
            ExternalActionGate::new(Duration::from_secs(60)),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1000),
        )
    }

    #[test]
    fn approves_exact_payload_once_and_shows_content() {
        let (g, now) = gate();
        let p = g.preview(&action(), true, now).unwrap();
        assert_eq!(p.payload, "secret body");
        assert_eq!(p.digest, action().digest());
        let ok = g.confirm(&p.id, &action(), true, now).unwrap();
        assert_eq!(ok.digest, p.digest);
        assert_eq!(g.confirm(&p.id, &action(), true, now), Err(Denial::Unknown));
    }

    #[test]
    fn changed_payload_or_target_rejected_and_burns_approval() {
        let (g, now) = gate();
        let p = g.preview(&action(), true, now).unwrap();
        let mut changed = action();
        changed.target = "mallory@example.com".into();
        assert_eq!(
            g.confirm(&p.id, &changed, true, now),
            Err(Denial::PayloadChanged)
        );
        assert_eq!(g.confirm(&p.id, &action(), true, now), Err(Denial::Unknown));
        let p = g.preview(&action(), true, now).unwrap();
        let mut changed = action();
        changed.payload.push('!');
        assert_eq!(
            g.confirm(&p.id, &changed, true, now),
            Err(Denial::PayloadChanged)
        );
    }

    #[test]
    fn field_boundaries_are_unambiguous() {
        let a = ExternalAction {
            target: "ab".into(),
            payload: "c".into(),
            ..action()
        };
        let b = ExternalAction {
            target: "a".into(),
            payload: "bc".into(),
            ..action()
        };
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn expired_is_denied() {
        let (g, now) = gate();
        let p = g.preview(&action(), true, now).unwrap();
        let later = now + Duration::from_secs(60);
        assert_eq!(
            g.confirm(&p.id, &action(), true, later),
            Err(Denial::Expired)
        );
    }

    #[test]
    fn noninteractive_is_denied() {
        let (g, now) = gate();
        assert_eq!(
            g.preview(&action(), false, now).unwrap_err(),
            Denial::Noninteractive
        );
        let p = g.preview(&action(), true, now).unwrap();
        assert_eq!(
            g.confirm(&p.id, &action(), false, now),
            Err(Denial::Noninteractive)
        );
        assert!(g.confirm(&p.id, &action(), true, now).is_ok());
    }

    #[test]
    fn wrong_owner_is_denied_without_consuming() {
        let (g, now) = gate();
        let p = g.preview(&action(), true, now).unwrap();
        let mut other = action();
        other.owner = "eve".into();
        assert_eq!(g.confirm(&p.id, &other, true, now), Err(Denial::WrongOwner));
        assert!(g.confirm(&p.id, &action(), true, now).is_ok());
    }

    #[test]
    fn debug_and_errors_omit_content() {
        let (g, now) = gate();
        let p = g.preview(&action(), true, now).unwrap();
        let out = format!("{:?} {:?} {}", p, action(), Denial::PayloadChanged);
        assert!(!out.contains("secret body") && !out.contains("bob@example.com"));
    }
}
