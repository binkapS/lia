//! Approval policy for tool execution.
//!
//! The assistant runs commands on the host, so the default is to ask before
//! anything that could damage the machine. The policy is consulted by the
//! agent loop before a risky tool runs; a channel that cannot prompt (a
//! non-interactive one-shot run) falls back to the policy's configured
//! fallback.

use serde::{Deserialize, Serialize};

/// How the assistant should treat a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Never run a tool that requires approval.
    Deny,
    /// Always run it without asking.
    Allow,
    /// Ask the user first.
    #[default]
    Ask,
}

/// A pending request for permission to run a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Tool being called.
    pub tool: String,
    /// Human-readable summary of what it will do.
    pub summary: String,
    /// The arguments, formatted for display.
    pub arguments: String,
    /// Where the request came from (session id, channel, ...).
    pub session_id: String,
}

impl ApprovalRequest {
    pub fn new(
        tool: impl Into<String>,
        summary: impl Into<String>,
        arguments: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        ApprovalRequest {
            tool: tool.into(),
            summary: summary.into(),
            arguments: arguments.into(),
            session_id: session_id.into(),
        }
    }
}

/// What the user decided about a pending approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
    /// Approve this and every similar call for the rest of the session.
    ApproveAlways,
}

/// What should happen to a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Run it without asking.
    Run,
    /// Ask the user first.
    Ask,
    /// Do not run it.
    Deny,
}

/// The assistant's tool-approval configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalConfig {
    /// Policy applied to tools that require approval.
    pub policy: ApprovalPolicy,
    /// Tool names that never require approval, regardless of policy.
    #[serde(default)]
    pub always_allowed: Vec<String>,
    /// Tool names that are always denied.
    #[serde(default)]
    pub always_denied: Vec<String>,
    /// What to do when nobody can be asked (non-interactive session).
    #[serde(default)]
    pub on_no_interaction: ApprovalPolicy,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        ApprovalConfig {
            policy: ApprovalPolicy::Ask,
            always_allowed: vec!["read_file".into(), "list_directory".into()],
            always_denied: Vec::new(),
            on_no_interaction: ApprovalPolicy::Deny,
        }
    }
}

impl ApprovalConfig {
    /// Decide what to do with a tool call.
    ///
    /// `interactive` tells us whether there is someone available to ask. The
    /// standing grants and denials win over the policy; after that, a session
    /// that cannot prompt falls back to `on_no_interaction`, so a headless
    /// run never silently assumes yes.
    pub fn evaluate(&self, tool: &str, interactive: bool) -> Plan {
        if self.always_denied.iter().any(|t| t == tool) {
            return Plan::Deny;
        }
        if self.always_allowed.iter().any(|t| t == tool) {
            return Plan::Run;
        }

        let effective = if interactive {
            self.policy
        } else {
            self.on_no_interaction
        };

        match effective {
            ApprovalPolicy::Allow => Plan::Run,
            ApprovalPolicy::Deny => Plan::Deny,
            ApprovalPolicy::Ask => {
                if interactive {
                    Plan::Ask
                } else {
                    Plan::Deny
                }
            }
        }
    }

    /// Grant a one-time or session-wide exception after the user says yes.
    pub fn allow_tool(&mut self, tool: &str, always: bool) {
        self.always_denied.retain(|t| t != tool);
        if always && !self.always_allowed.iter().any(|t| t == tool) {
            self.always_allowed.push(tool.to_string());
        }
    }

    pub fn deny_tool(&mut self, tool: &str) {
        self.always_allowed.retain(|t| t != tool);
        if !self.always_denied.iter().any(|t| t == tool) {
            self.always_denied.push(tool.to_string());
        }
    }
}
