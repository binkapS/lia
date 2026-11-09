//! The agent loop.
//!
//! A turn looks like this:
//!
//! ```text
//!   messages -> provider -> (text | tool calls)
//!                          |              |
//!                          |              +-> run tools -> append results -> loop
//!                          +-> done
//! ```
//!
//! The loop is bounded by a max-iteration count, and it compacts the
//! conversation when it approaches the model's context window so long
//! sessions do not die on overflow.

pub mod compaction;
pub mod session;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use crate::provider::types::{
    CompletionRequest, ContentBlock, Message, StopReason, StreamEvent, ToolSpec, Usage,
};
use crate::provider::{Provider, ProviderError};
use crate::tool::approval::{ApprovalConfig, ApprovalDecision, Plan};
use crate::tool::{ToolContext, ToolOutput, ToolRegistry};

pub use session::{Session, SessionStore};

/// Events emitted while a turn runs, so the UI can render progress live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The model is starting a response.
    Started { model: String },
    /// A chunk of assistant text.
    TextDelta(String),
    /// A chunk of the model's thinking.
    ThinkingDelta(String),
    /// The model asked for a tool to run.
    ToolCallStarted {
        id: String,
        name: String,
        input: Value,
    },
    /// A tool finished.
    ToolCallFinished {
        id: String,
        name: String,
        output: ToolOutput,
    },
    /// The assistant turn is complete.
    TurnComplete {
        text: String,
        usage: Option<Usage>,
        stop_reason: Option<StopReason>,
    },
    /// The whole turn is done.
    Finished {
        messages: Vec<Message>,
        usage: Option<Usage>,
        iterations: usize,
    },
    /// What another assistant is doing, in the conversation that handed it
    /// its work.
    SubAssistant {
        /// A few words saying what the work is.
        label: String,
        /// What it is doing now.
        note: String,
    },
    /// Something worth showing but not fatal.
    Notice(String),
    /// A fatal error ended the turn.
    Error(String),
}

/// Configuration for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Hard cap on provider round-trips per turn.
    pub max_iterations: usize,
    /// Whether the assistant may call tools at all.
    pub tools_enabled: bool,
    /// System prompt sent with every request.
    pub system_prompt: Option<String>,
    /// Approval configuration for risky tools.
    pub approval: ApprovalConfig,
    /// Fraction of the context window at which to compact.
    pub compaction_threshold: f64,
    /// Maximum tokens the model may generate per response.
    pub max_output_tokens: Option<usize>,
    /// The model id the agent uses, for lightweight follow-up calls such as
    /// naming a session.
    pub model: Option<String>,
    pub temperature: Option<f64>,
    /// Size of the model's context window in tokens. Used to decide when to
    /// compact. Falls back to a conservative default when unknown.
    pub context_window: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            max_iterations: 25,
            tools_enabled: true,
            system_prompt: None,
            approval: ApprovalConfig::default(),
            compaction_threshold: 0.8,
            max_output_tokens: None,
            model: None,
            temperature: None,
            context_window: 128_000,
        }
    }
}

/// Handles approval requests for tools that need permission.
///
/// The TUI implements this to prompt the user; a non-interactive session
/// implements it to deny.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Resolve an approval request. `None` means the caller gave up waiting.
    async fn request(
        &self,
        request: crate::tool::approval::ApprovalRequest,
    ) -> Option<ApprovalDecision>;

    /// Whether this handler can actually ask somebody.
    ///
    /// A channel that cannot prompt says no, so the configured
    /// `on_no_interaction` policy decides instead of the interactive one.
    fn is_interactive(&self) -> bool {
        true
    }
}

/// An approval handler that approves everything.
pub struct AllowAll;

#[async_trait]
impl ApprovalHandler for AllowAll {
    async fn request(
        &self,
        _request: crate::tool::approval::ApprovalRequest,
    ) -> Option<ApprovalDecision> {
        Some(ApprovalDecision::Approve)
    }
}

/// An approval handler that denies everything.
pub struct DenyAll;

#[async_trait]
impl ApprovalHandler for DenyAll {
    async fn request(
        &self,
        _request: crate::tool::approval::ApprovalRequest,
    ) -> Option<ApprovalDecision> {
        Some(ApprovalDecision::Deny)
    }
}

/// An approval handler for sessions where nobody can be asked.
///
/// It refuses everything and reports itself as non-interactive, so the
/// `on_no_interaction` policy decides. A one-shot run should not assume
/// yes just because the model asked nicely.
pub struct NoInteraction;

#[async_trait]
impl ApprovalHandler for NoInteraction {
    async fn request(
        &self,
        _request: crate::tool::approval::ApprovalRequest,
    ) -> Option<ApprovalDecision> {
        Some(ApprovalDecision::Deny)
    }

    fn is_interactive(&self) -> bool {
        false
    }
}

/// Notices a tool call that keeps failing the same way.
///
/// A model that calls the same tool with the same arguments and gets the
/// same failure is not making progress. Three of those in a row is a loop,
/// and the turn should end rather than spend the iteration budget on it.
#[derive(Debug, Default)]
struct FailureTracker {
    /// The call that failed last, and how many times in a row.
    last: Option<(String, String)>,
    count: usize,
}

impl FailureTracker {
    /// How many identical failures end the turn.
    const LIMIT: usize = 3;

    /// Record a call and its outcome.
    ///
    /// Returns true when the same call has now failed [`FailureTracker::LIMIT`]
    /// times in a row.
    fn record(&mut self, name: &str, input: &Value, failed: bool) -> bool {
        if !failed {
            // A call that worked means the model moved on.
            self.last = None;
            self.count = 0;
            return false;
        }

        let signature = (name.to_string(), input.to_string());
        match &self.last {
            Some(previous) if *previous == signature => self.count += 1,
            _ => {
                self.last = Some(signature);
                self.count = 1;
            }
        }

        self.count >= Self::LIMIT
    }
}

/// The agent: a provider, a set of tools, and the loop that drives them.
pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    config: AgentConfig,
    /// Tools the user approved for the rest of a session, keyed by session.
    grants: Arc<Mutex<HashMap<String, HashSet<String>>>>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, tools: Arc<ToolRegistry>, config: AgentConfig) -> Self {
        Agent {
            provider,
            tools,
            config,
            grants: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Run a full turn, streaming events to `tx` as they happen.
    ///
    /// The final message list is returned so the caller can persist it.
    pub async fn run_turn(
        &self,
        messages: Vec<Message>,
        session_id: &str,
        approver: Option<&dyn ApprovalHandler>,
        mut tx: mpsc::Sender<AgentEvent>,
    ) -> (Vec<Message>, Option<Usage>, usize) {
        let mut messages = messages;
        let mut total_usage: Option<Usage> = None;
        let mut iterations = 0usize;
        let mut failures = FailureTracker::default();
        // Whether the pictures have already been given up on, so the turn
        // only tries that once.
        let mut dropped_images = false;
        // Whether we have already tried compacting after an overflow, so a
        // second rejection is surfaced instead of looping forever.
        let mut overflow_retried = false;

        // What the turn knows about itself. The user's message is the last
        // user turn, and it is what the assistant is answering.
        let mut context = ToolContext {
            tainted: false,
            user_text: messages
                .iter()
                .rev()
                .find(|m| m.role == crate::provider::types::Role::User)
                .map(|m| m.text())
                .unwrap_or_default(),
            session_id: session_id.to_string(),
        };

        let _ = tx
            .send(AgentEvent::Started {
                model: self.provider.model().to_string(),
            })
            .await;

        loop {
            if iterations >= self.config.max_iterations {
                let _ = tx
                    .send(AgentEvent::Notice(format!(
                        "stopping after {} iterations (max reached)",
                        self.config.max_iterations
                    )))
                    .await;
                break;
            }
            iterations += 1;

            // Trim the conversation if it is close to the context limit.
            let request = self.build_request(&messages, session_id);
            if let Some(compacted) = compaction::should_compact(&request, &self.config) {
                messages = compaction::compact(messages, compacted, self.provider.as_ref()).await;
                let _ = tx
                    .send(AgentEvent::Notice(
                        "compacted conversation to fit the context window".into(),
                    ))
                    .await;
            }

            let stream = match self.provider.stream(&request).await {
                Ok(s) => s,
                Err(e) => {
                    // When the provider rejects the request as too long, compact
                    // the conversation once and retry before giving up.
                    if !overflow_retried && matches!(e, ProviderError::ContextOverflow(_)) {
                        overflow_retried = true;
                        messages = compaction::compact(messages, 0, self.provider.as_ref()).await;
                        let _ = tx
                            .send(AgentEvent::Notice(
                                "compacted conversation to fit the context window".into(),
                            ))
                            .await;
                        continue;
                    }
                    // A model that cannot see is still a model that can be
                    // asked: the picture is described in words instead of
                    // being sent, and the turn carries on.
                    if !dropped_images && carries_an_image(&messages) && refuses_images(&e) {
                        dropped_images = true;
                        messages = describe_images(&messages);
                        let _ = tx
                            .send(AgentEvent::Notice(
                                "this model cannot see pictures — they are described instead"
                                    .into(),
                            ))
                            .await;
                        continue;
                    }
                    let message = self.describe_error(&e);
                    let _ = tx.send(AgentEvent::Error(message.clone())).await;
                    return (messages, total_usage, iterations);
                }
            };

            // Collect the streamed response into a message.
            let (assistant_message, stop_reason, usage) =
                self.collect_stream(stream, &mut tx).await;

            if let Some(usage) = usage {
                total_usage = Some(match total_usage {
                    Some(prev) => Usage {
                        input_tokens: prev.input_tokens + usage.input_tokens,
                        output_tokens: prev.output_tokens + usage.output_tokens,
                    },
                    None => usage,
                });
            }

            if assistant_message.content.is_empty() {
                // The model produced nothing; stop rather than spin.
                if stop_reason != Some(StopReason::ToolUse) {
                    break;
                }
            }

            let text = assistant_message.text();
            let tool_calls: Vec<ContentBlock> = assistant_message
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .cloned()
                .collect();

            let _ = tx
                .send(AgentEvent::TurnComplete {
                    text: text.clone(),
                    usage,
                    stop_reason,
                })
                .await;

            messages.push(assistant_message);

            if tool_calls.is_empty() {
                break;
            }

            // Run every requested tool, then feed the results back.
            let mut looping = false;
            for call in tool_calls {
                let ContentBlock::ToolUse { id, name, input } = call else {
                    continue;
                };

                let _ = tx
                    .send(AgentEvent::ToolCallStarted {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    })
                    .await;

                let output = self
                    .execute_tool(&id, &name, &input, session_id, approver, &context)
                    .await;

                // The same call failing the same way, over and over, is a
                // loop rather than progress. Stop before it burns the
                // iteration budget.
                if failures.record(&name, &input, output.is_error) {
                    let _ = tx
                        .send(AgentEvent::Notice(format!(
                            "stopping: '{name}' failed {} times in the same way",
                            failures.count
                        )))
                        .await;
                    looping = true;
                }

                // A result from outside the machine taints the rest of the
                // turn: anything remembered after this is not the user's
                // word, and cannot be treated as a standing instruction.
                // The answer of another assistant counts too: it may have
                // read anything.
                if output
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("source"))
                    .and_then(Value::as_str)
                    .is_some_and(is_untrusted_source)
                {
                    context.tainted = true;
                }

                let _ = tx
                    .send(AgentEvent::ToolCallFinished {
                        id: id.clone(),
                        name: name.clone(),
                        output: output.clone(),
                    })
                    .await;

                messages.push(Message {
                    role: crate::provider::types::Role::Tool,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: output.content,
                        is_error: output.is_error,
                    }],
                });
            }

            if looping {
                break;
            }
        }

        let _ = tx
            .send(AgentEvent::Finished {
                messages: messages.clone(),
                usage: total_usage,
                iterations,
            })
            .await;

        (messages, total_usage, iterations)
    }

    /// Assemble the request for one round-trip.
    fn build_request(&self, messages: &[Message], session_id: &str) -> CompletionRequest {
        let tools: Vec<ToolSpec> = if self.config.tools_enabled {
            self.tools.specs_for(session_id)
        } else {
            Vec::new()
        };

        CompletionRequest {
            model: self.provider.model().to_string(),
            system: self.config.system_prompt.clone(),
            messages: messages.to_vec(),
            tools,
            max_output_tokens: self.config.max_output_tokens,
            temperature: self.config.temperature,
            extra: None,
        }
    }

    /// Run one tool, honouring the approval policy.
    async fn execute_tool(
        &self,
        _id: &str,
        name: &str,
        input: &Value,
        session_id: &str,
        approver: Option<&dyn ApprovalHandler>,
        context: &ToolContext,
    ) -> ToolOutput {
        let Some(tool) = self.tools.get(name) else {
            return ToolOutput::error(format!("unknown tool: {name}"));
        };

        if tool.requires_approval() {
            match self
                .resolve_approval(name, tool.description(), input, session_id, approver)
                .await
            {
                ApprovalOutcome::Approved => {}
                ApprovalOutcome::Granted => {
                    // The user said "always", so later calls in this session
                    // will not ask again.
                }
                ApprovalOutcome::DeniedByPolicy => {
                    return ToolOutput::error(format!(
                        "the '{name}' tool is not allowed by the approval policy, so it did not run"
                    ));
                }
                ApprovalOutcome::DeniedByUser => {
                    return ToolOutput::error(format!(
                        "the user denied the '{name}' tool call, so it did not run"
                    ));
                }
            }
        }

        // The tool sees what the agent knows about the turn, so it does not
        // have to guess.
        let input = tool.with_context(input.clone(), context);

        match tool.run(input).await {
            Ok(output) => output,
            Err(e) => ToolOutput::error(e.to_string()),
        }
    }

    /// Work out whether a risky tool may run, asking the user if needed.
    async fn resolve_approval(
        &self,
        name: &str,
        description: &str,
        input: &Value,
        session_id: &str,
        approver: Option<&dyn ApprovalHandler>,
    ) -> ApprovalOutcome {
        let plan = self
            .config
            .approval
            .evaluate(name, approver.is_some_and(|a| a.is_interactive()));

        match plan {
            Plan::Run => return ApprovalOutcome::Approved,
            Plan::Deny => return ApprovalOutcome::DeniedByPolicy,
            Plan::Ask => {}
        }

        // A grant from earlier in this session stands in for another prompt.
        if self.is_granted(session_id, name).await {
            return ApprovalOutcome::Approved;
        }

        let Some(handler) = approver else {
            return ApprovalOutcome::DeniedByPolicy;
        };

        let request = crate::tool::approval::ApprovalRequest::new(
            name,
            description,
            serde_json::to_string_pretty(input).unwrap_or_default(),
            session_id,
        );

        match handler.request(request).await {
            Some(ApprovalDecision::Approve) => ApprovalOutcome::Approved,
            Some(ApprovalDecision::ApproveAlways) => {
                self.grant(session_id, name).await;
                ApprovalOutcome::Granted
            }
            Some(ApprovalDecision::Deny) | None => ApprovalOutcome::DeniedByUser,
        }
    }

    /// Whether the user has approved this tool for the whole session.
    async fn is_granted(&self, session_id: &str, tool: &str) -> bool {
        self.grants
            .lock()
            .await
            .get(session_id)
            .is_some_and(|set| set.contains(tool))
    }

    /// Remember that the user approved this tool for the whole session.
    async fn grant(&self, session_id: &str, tool: &str) {
        self.grants
            .lock()
            .await
            .entry(session_id.to_string())
            .or_default()
            .insert(tool.to_string());
    }

    /// Drain a provider stream into an assistant message.
    async fn collect_stream(
        &self,
        mut stream: crate::provider::EventStream,
        tx: &mut mpsc::Sender<AgentEvent>,
    ) -> (Message, Option<StopReason>, Option<Usage>) {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut tool_calls: Vec<ContentBlock> = Vec::new();
        // Tool arguments arrive as JSON fragments and are assembled here.
        let mut pending_args: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut pending_names: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut stop_reason = None;
        let mut usage = None;

        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamEvent::Text(delta)) => {
                    text.push_str(&delta);
                    let _ = tx.send(AgentEvent::TextDelta(delta)).await;
                }
                Ok(StreamEvent::Thinking(delta)) => {
                    thinking.push_str(&delta);
                    let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;
                }
                Ok(StreamEvent::ToolCallStart { id, name }) => {
                    pending_names.insert(id.clone(), name);
                }
                Ok(StreamEvent::ToolCallDelta { id, arguments }) => {
                    pending_args.entry(id).or_default().push_str(&arguments);
                }
                Ok(StreamEvent::ToolCallEnd { id }) => {
                    let name = pending_names.get(&id).cloned().unwrap_or_default();
                    let raw = pending_args.remove(&id).unwrap_or_default();
                    let input = if raw.trim().is_empty() {
                        Value::Object(Default::default())
                    } else {
                        serde_json::from_str(&raw).unwrap_or_else(|_| {
                            Value::Object(
                                [("_raw".to_string(), Value::String(raw))]
                                    .into_iter()
                                    .collect(),
                            )
                        })
                    };
                    tool_calls.push(ContentBlock::ToolUse { id, name, input });
                }
                Ok(StreamEvent::Done {
                    stop_reason: s,
                    usage: u,
                }) => {
                    stop_reason = s.or(stop_reason);
                    usage = u.or(usage);
                }
                Ok(StreamEvent::Ping) => {}
                Err(e) => {
                    let message = self.describe_error(&e);
                    let _ = tx.send(AgentEvent::Error(message)).await;
                    break;
                }
            }
        }

        let mut content = Vec::new();
        if !thinking.is_empty() {
            content.push(ContentBlock::Thinking { text: thinking });
        }
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
        content.extend(tool_calls);

        (
            Message {
                role: crate::provider::types::Role::Assistant,
                content,
            },
            stop_reason,
            usage,
        )
    }

    /// Turn a provider error into something worth showing a user.
    fn describe_error(&self, error: &ProviderError) -> String {
        match error {
            ProviderError::Auth(body) => format!(
                "authentication failed — check the API key in your config. {}",
                truncate(body, 200)
            ),
            ProviderError::RateLimited(body) => {
                format!("rate limited by the provider. {}", truncate(body, 200))
            }
            ProviderError::ContextOverflow(body) => format!(
                "the request exceeded the model's context window. {}",
                truncate(body, 200)
            ),
            ProviderError::ModelNotFound(body) => format!(
                "the model '{}' is not available. {}",
                self.provider.model(),
                truncate(body, 200)
            ),
            ProviderError::Network(e) => format!("could not reach the provider: {e}"),
            ProviderError::Decode(detail) => {
                format!("could not read the provider's reply: {detail}")
            }
            ProviderError::InvalidRequest(body) => {
                format!("the provider rejected the request. {}", truncate(body, 200))
            }
            ProviderError::Config(detail) => format!("configuration problem: {detail}"),
            ProviderError::Http { status, body } => {
                format!("provider returned HTTP {status}. {}", truncate(body, 200))
            }
        }
    }
}

/// Shorten a string for display in an error message.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max {
        trimmed.to_string()
    } else {
        let mut end = max;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

/// The result of asking whether a risky tool may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalOutcome {
    /// The user said yes.
    Approved,
    /// The user said "yes, and stop asking".
    Granted,
    /// The configuration forbids the tool.
    DeniedByPolicy,
    /// The user said no.
    DeniedByUser,
}

/// Whether a result came from somewhere the user cannot vouch for.
///
/// The network is the obvious one. Another assistant's answer is the other:
/// it may have read a page, and said what the page said.
fn is_untrusted_source(source: &str) -> bool {
    matches!(source, "network" | "assistant")
}

/// The agent plus the state that belongs to a single conversation.
///
/// Sessions own the message history, so the UI and the channels can share one.
pub struct SessionRunner {
    agent: Arc<Agent>,
    sessions: Arc<Mutex<SessionStore>>,
    approvers: Arc<Mutex<std::collections::HashMap<String, Arc<dyn ApprovalHandler>>>>,
    /// Where the events of each session are sent, so work handed to another
    /// assistant can be seen in the conversation that handed it over.
    senders: Arc<Mutex<std::collections::HashMap<String, mpsc::Sender<AgentEvent>>>>,
}

impl SessionRunner {
    /// Build the runner, and tell the task tool where to run work.
    pub fn new(agent: Arc<Agent>, sessions: Arc<Mutex<SessionStore>>) -> Arc<SessionRunner> {
        let runner = Arc::new(SessionRunner {
            agent,
            sessions,
            approvers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            senders: Arc::new(Mutex::new(std::collections::HashMap::new())),
        });

        // The tool that hands work to another assistant runs it here. The
        // handle is weak, so the tool does not keep the runner alive.
        let as_runner: Arc<dyn crate::tool::task::TaskRunner> = runner.clone();
        let handle = crate::tool::task::TaskRunnerHandle::new(&as_runner);
        if let Some(tool) = runner.agent.tools().get("task") {
            tool.set_task_runner(handle);
        }

        runner
    }

    /// Register a handler that can approve tools for a session.
    pub async fn set_approver(&self, session_id: &str, handler: Arc<dyn ApprovalHandler>) {
        self.approvers
            .lock()
            .await
            .insert(session_id.to_string(), handler);
    }

    /// The messages of a session, oldest first.
    pub async fn session_messages(&self, session_id: &str) -> Option<Vec<Message>> {
        let store = self.sessions.lock().await;
        store
            .get(session_id)
            .map(|session| session.messages.clone())
    }

    /// The saved sessions, newest first.
    pub async fn list_sessions(&self) -> Vec<(String, String)> {
        let store = self.sessions.lock().await;
        store
            .list()
            .iter()
            .map(|session| (session.id.clone(), session.title.clone()))
            .collect()
    }

    /// Forget a session's history, so the next message starts afresh.
    ///
    /// The session keeps its id, so the conversation it belongs to carries on
    /// — but the model no longer remembers what was said before. Returns
    /// whether there was anything to forget.
    pub async fn reset_session(&self, session_id: &str) -> bool {
        let saved = {
            let mut store = self.sessions.lock().await;
            let dir = store.dir().map(|d| d.to_path_buf());
            match store.get_mut(session_id) {
                Some(session) if !session.messages.is_empty() => {
                    session.messages.clear();
                    session.title = "New session".to_string();
                    session.touch();
                    Some((dir, session.clone()))
                }
                _ => None,
            }
        };
        let Some((dir, snapshot)) = saved else {
            return false;
        };
        crate::agent::session::SessionStore::save_off_runtime(dir, snapshot).await;
        true
    }

    /// Create a brand-new empty session with the given id, persisting it.
    /// Returns whether it was actually created (false means it already
    /// existed).
    pub async fn create_session(&self, session_id: &str) -> bool {
        self.create_session_with_source(session_id, "tui").await
    }

    /// Like `create_session`, but with an explicit origin label.
    pub async fn create_session_with_source(&self, session_id: &str, source: &str) -> bool {
        let saved = {
            let mut store = self.sessions.lock().await;
            if store.get(session_id).is_some() {
                return false;
            }
            let mut session = Session::new(session_id.to_string(), source);
            session.title = "New session".to_string();
            store.insert(session.clone());
            Some((store.dir().map(|d| d.to_path_buf()), session))
        };
        if let Some((dir, session)) = saved {
            crate::agent::session::SessionStore::save_off_runtime(dir, session).await;
        }
        true
    }

    /// Back-compatible name for creating a session.
    pub async fn new_session(&self, session_id: &str) -> bool {
        self.create_session(session_id).await
    }

    /// Ask the model for a short name for this session.
    async fn suggest_session_title(&self, messages: &[Message]) -> Option<String> {
        let mut transcript = String::new();
        for m in messages.iter().take(8) {
            let role = match m.role {
                crate::provider::types::Role::User => "User",
                crate::provider::types::Role::Assistant => "Assistant",
                _ => continue,
            };
            let text = m.text().trim().replace('\n', " ");
            if !text.is_empty() {
                transcript.push_str(&format!("{role}: {}\n", text.chars().take(200).collect::<String>()));
            }
        }
        if transcript.trim().is_empty() {
            return None;
        }
        let model = self.agent.config().model.clone().unwrap_or_default();
        if model.is_empty() {
            return None;
        }
        let request = crate::provider::types::CompletionRequest {
            model,
            system: Some(
                "Name this chat session in three to six words. Reply with the title only.".to_string(),
            ),
            messages: vec![Message::user(transcript)],
            max_output_tokens: Some(24),
            ..Default::default()
        };
        let response = self.agent.provider().complete(&request).await.ok()?;
        let mut title = response.text().trim().to_string();
        title = title.trim_matches('"').trim_matches('\'').trim().to_string();
        if title.is_empty() { None } else { Some(title) }
    }

    /// Run a turn for a session, appending the result to its history.
    ///
    /// Returns what the assistant said, so a caller that is not watching
    /// the events — a scheduled job — still gets the answer.
    pub async fn run(
        &self,
        session_id: &str,
        user_input: Option<String>,
        tx: mpsc::Sender<AgentEvent>,
    ) -> Option<String> {
        let message = user_input.map(Message::user);
        self.run_message(session_id, message, tx).await
    }

    /// Run a turn from a message that may carry pictures.
    pub async fn run_message(
        &self,
        session_id: &str,
        user_input: Option<Message>,
        tx: mpsc::Sender<AgentEvent>,
    ) -> Option<String> {
        // Remember where the events of this session go, so work handed to
        // another assistant can be shown in the same transcript.
        self.senders
            .lock()
            .await
            .insert(session_id.to_string(), tx.clone());

        let mut store = self.sessions.lock().await;
        let session = store.get_or_create(session_id)?;
        if let Some(input) = user_input {
            session.messages.push(input);
        }
        let messages = session.messages.clone();
        drop(store);

        let approver = self.approvers.lock().await.get(session_id).cloned();

        let (messages, _, _) = self
            .agent
            .run_turn(messages, session_id, approver.as_deref(), tx.clone())
            .await;

        // The turn is over, so the events of this session have nowhere to
        // go: the queue is let go, so whoever reads it sees the end.
        {
            let mut senders = self.senders.lock().await;
            if let Some(stored) = senders.get(session_id)
                && stored.same_channel(&tx)
            {
                senders.remove(session_id);
            }
        }

        // The answer is the last thing the assistant said.
        let answer = messages
            .iter()
            .rev()
            .find(|m| m.role == crate::provider::types::Role::Assistant)
            .map(|m| m.text())
            .filter(|text| !text.trim().is_empty());

        // The session is saved as the conversation happens, so it survives a
        // restart and can be picked up from the session list later. The
        // snapshot is made under the sessions lock, but the write happens
        // off the runtime: rewriting the whole history file is disk work
        // that must not hold the lock everybody else reads through.
        let needs_title = {
            let store = self.sessions.lock().await;
            store
                .get(session_id)
                .map(|s| s.title == "New session")
                .unwrap_or(false)
        };
        let suggested_title = if needs_title {
            self.suggest_session_title(&messages).await
        } else {
            None
        };

        let saved = {
            let mut store = self.sessions.lock().await;
            let dir = store.dir().map(|d| d.to_path_buf());
            match store.get_mut(session_id) {
                Some(session) => {
                    session.messages = messages;
                    session.touch();
                    // A session that has never been named gets a short title
                    // from the model; falling back to the first message when
                    // the model cannot be asked.
                    if session.title == "New session" {
                        if let Some(title) = suggested_title {
                            session.title = format!("Lia | {title}");
                        } else {
                            session.derive_title();
                        }
                    }
                    Some((dir, session.clone()))
                }
                None => None,
            }
        };

        if let Some((dir, snapshot)) = saved {
            crate::agent::session::SessionStore::save_off_runtime(dir, snapshot).await;
        }
        answer
    }
}

#[async_trait]
impl crate::tool::task::TaskRunner for SessionRunner {
    /// Run a task in a conversation of its own, and answer with what the
    /// assistant said.
    ///
    /// The other assistant starts with a clean context: it is given the
    /// brief and nothing else, so the conversation it is working in cannot
    /// crowd out the task. Its risky tools are answered by the same person
    /// who is talking to the main assistant, so permission means the same
    /// thing there as it does here.
    async fn run_task(
        &self,
        session_id: &str,
        prompt: &str,
        label: &str,
    ) -> crate::tool::Result<String> {
        let sub_session = format!(
            "{}{}",
            crate::tool::task::SUB_SESSION_PREFIX,
            uuid::Uuid::new_v4()
        );

        // The same person answers for the other assistant.
        let approver = self.approvers.lock().await.get(session_id).cloned();

        // The other assistant's work is shown in the conversation that
        // handed it over — one line for what it is doing, not the whole
        // performance — so the person waiting can see it is making
        // progress. Where the events go is remembered per session; without
        // a listener the events are dropped, as before.
        let listener = self.senders.lock().await.get(session_id).cloned();
        let (tx, rx) = mpsc::channel(64);
        let forwarder = match listener {
            Some(listener) => {
                let label = label.to_string();
                Some(tokio::spawn(async move {
                    let mut rx = rx;
                    while let Some(event) = rx.recv().await {
                        if let Some(note) = describe_sub_event(&event) {
                            let _ = listener
                                .send(AgentEvent::SubAssistant {
                                    label: label.clone(),
                                    note,
                                })
                                .await;
                        }
                    }
                }))
            }
            // If nobody is listening, the events are thrown away rather
            // than left to fill the queue.
            None => {
                drop(rx);
                None
            }
        };

        let messages = vec![Message::user(prompt.to_string())];
        let (messages, _, _) = self
            .agent
            .run_turn(messages, &sub_session, approver.as_deref(), tx)
            .await;

        // The forwarder ends when the queue closes, which is when the
        // other assistant's turn is over.
        if let Some(forwarder) = forwarder {
            let _ = forwarder.await;
        }

        // The answer is the last thing the other assistant said.
        messages
            .iter()
            .rev()
            .find(|m| m.role == crate::provider::types::Role::Assistant)
            .map(|m| m.text())
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| {
                crate::tool::ToolError::Failed(
                    "task".into(),
                    "the other assistant did not answer".into(),
                )
            })
    }
}

/// A line about what another assistant is doing, if the event is worth one.
fn describe_sub_event(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::ToolCallStarted { name, .. } => Some(format!("used {name}")),
        AgentEvent::TurnComplete { text, .. } => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(format!("said: {}", first_line(text, 160)))
            }
        }
        AgentEvent::Error(message) => Some(format!("hit a problem: {message}")),
        _ => None,
    }
}

/// The first line of a text, shortened.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// Whether the conversation carries a picture.
fn carries_an_image(messages: &[Message]) -> bool {
    messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    })
}

/// Replace every picture with a line of text saying what it was.
fn describe_images(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|message| Message {
            role: message.role,
            content: message
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Image { media_type, data } => ContentBlock::text(format!(
                        "[a picture was attached ({media_type}, {} kB)]",
                        data.len() * 3 / 4 / 1024
                    )),
                    other => other.clone(),
                })
                .collect(),
        })
        .collect()
}

/// Whether the provider's complaint is about the pictures.
fn refuses_images(error: &ProviderError) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("image") || text.contains("vision") || text.contains("multimodal")
}

#[cfg(test)]
mod tests {
    use super::is_untrusted_source;

    #[test]
    fn what_came_from_outside_is_not_the_user_s_word() {
        assert!(is_untrusted_source("network"));
        assert!(is_untrusted_source("assistant"));
        assert!(!is_untrusted_source("local"));
    }
}
