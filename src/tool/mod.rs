//! Tool registry and execution.
//!
//! A tool is anything the model can invoke: reading a file, running a shell
//! command, searching the filesystem, fetching a URL. Each tool declares a
//! JSON Schema for its parameters and implements [`Tool::run`].
//!
//! Tools execute on the host, so risky ones are gated behind an approval
//! policy (see [`ApprovalPolicy`]) — the same "untrusted execution, policy
//! enforced by code" boundary the reference design uses.

pub mod approval;
pub mod document;
pub mod fetch;
pub mod filesystem;
pub mod memory;
pub mod personal;
pub mod sandbox;
pub mod schedule;
pub mod search;
pub mod shell;
pub mod system;
pub mod task;
pub mod todo;
pub mod voice;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::provider::types::ToolSpec;

pub use approval::{ApprovalPolicy, ApprovalRequest};
pub use sandbox::{Sandbox, SandboxConfig};

/// What a tool produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Text handed back to the model.
    pub content: String,
    /// Whether the tool failed. Failures are reported to the model so it can
    /// recover rather than aborting the whole turn.
    pub is_error: bool,
    /// Optional structured data for the UI (diff stats, exit codes, ...).
    pub metadata: Option<Value>,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: false,
            metadata: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: true,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    Unknown(String),

    #[error("invalid arguments for {tool}: {reason}")]
    InvalidArgs { tool: String, reason: String },

    #[error("tool `{0}` failed: {1}")]
    Failed(String, String),

    #[error("tool `{0}` was denied by the user")]
    Denied(String),

    #[error("tool `{0}` timed out")]
    Timeout(String),
}

pub type Result<T> = std::result::Result<T, ToolError>;

/// What the agent knows about the turn a tool call belongs to.
///
/// Some tools care about more than their arguments: where a fact came from,
/// for instance. The agent passes this along so the tool does not have to
/// guess.
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    /// Whether a result from outside the machine has been seen this turn.
    pub tainted: bool,
    /// The user's message for this turn, if there was one.
    pub user_text: String,
    /// The session the turn belongs to.
    ///
    /// A session id is `channel:conversation` for a chat, so a tool that
    /// needs to know where it was called from — a scheduled job that
    /// answers in the conversation that asked for it — can work it out.
    pub session_id: String,
}

/// A callable tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name the model refers to this tool by.
    fn name(&self) -> &str;

    /// Human-readable description shown to the model and in the TUI.
    fn description(&self) -> &str;

    /// JSON Schema describing the accepted parameters.
    fn parameters(&self) -> Value;

    /// Execute the tool with the given arguments.
    async fn run(&self, args: Value) -> Result<ToolOutput>;

    /// Annotate a call with what the agent knows about the turn.
    ///
    /// The default leaves the arguments alone. Tools that record where a
    /// fact came from override this, because the model should not be the one
    /// deciding that.
    fn with_context(&self, args: Value, _context: &ToolContext) -> Value {
        args
    }

    /// Whether the model should see this tool in a given session.
    ///
    /// Tools can be filtered per session — a read-only session should not
    /// advertise shell access.
    fn is_enabled(&self, _session_id: &str) -> bool {
        true
    }

    /// Whether this tool needs user approval before running.
    fn requires_approval(&self) -> bool {
        false
    }

    /// Tell the tool where to run work of its own, if it wants any.
    ///
    /// A tool that hands work to a sub-assistant needs somewhere to send
    /// it. The handle does not keep the runner alive, so a tool cannot
    /// keep the agent alive by accident.
    fn set_task_runner(&self, _runner: task::TaskRunnerHandle) {}

    /// The spec sent to the provider.
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(self.name(), self.description(), self.parameters())
    }
}

/// Everything the agent can call, keyed by name.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Tools excluded from every session, by name.
    disabled: Vec<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            tools: BTreeMap::new(),
            disabled: Vec::new(),
        }
    }

    /// Register a tool, replacing any tool with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn unregister(&mut self, name: &str) {
        self.tools.remove(name);
        self.disabled.retain(|n| n != name);
    }

    /// Exclude a tool from all sessions without removing it.
    pub fn disable(&mut self, name: &str) {
        if !self.disabled.iter().any(|n| n == name) {
            self.disabled.push(name.to_string());
        }
    }

    pub fn enable(&mut self, name: &str) {
        self.disabled.retain(|n| n != name);
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.iter().any(|n| n == name)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if self.is_disabled(name) {
            return None;
        }
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools
            .keys()
            .filter(|n| !self.is_disabled(n))
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Specs for the tools available in a session.
    pub fn specs_for(&self, session_id: &str) -> Vec<ToolSpec> {
        self.tools
            .values()
            .filter(|t| !self.is_disabled(t.name()) && t.is_enabled(session_id))
            .map(|t| t.spec())
            .collect()
    }

    /// Run a tool by name.
    pub async fn run(&self, name: &str, args: Value) -> Result<ToolOutput> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        let started = std::time::Instant::now();
        let result = tool.run(args).await;
        let elapsed = started.elapsed();
        tracing::info!(tool = name, elapsed_ms = elapsed.as_millis(), "tool invocation");
        result
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
