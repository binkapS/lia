//! Provider-agnostic message and request types.
//!
//! Every backend (Anthropic, OpenAI-compatible, local) is translated into and
//! out of these types, so the agent loop never needs to know which provider it
//! is talking to.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// A message carrying the result of a tool invocation.
    Tool,
}

/// A piece of message content.
///
/// Anthropic models content as a list of blocks; OpenAI-compatible APIs use a
/// flat string plus a separate `tool_calls` array. We keep the block form
/// internally and flatten it per-provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// The model thinking out loud, before its answer.
    ///
    /// A model that reasons says so separately from what it finally
    /// answers, and the two are kept apart: the thinking is worth showing,
    /// but it is not the answer.
    Thinking {
        text: String,
    },
    /// The model asking for a tool to run.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// The result of running a tool, fed back to the model.
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// An image passed in by the user. `media_type` is e.g. `image/png`.
    Image {
        media_type: String,
        data: String,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, ContentBlock::Text { .. })
    }

    /// Concatenation of every text block, ignoring tool blocks.
    pub fn text_content(&self) -> String {
        match self {
            ContentBlock::Text { text } => text.clone(),
            _ => String::new(),
        }
    }
}

/// A single turn in a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// A user turn that carries pictures as well as words.
    pub fn user_with_images(text: impl Into<String>, images: Vec<ContentBlock>) -> Self {
        let mut content = vec![ContentBlock::text(text)];
        content.extend(images);
        Message {
            role: Role::User,
            content,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// An assistant turn that only requests tool calls.
    pub fn assistant_tool_use(calls: Vec<ContentBlock>) -> Self {
        Message {
            role: Role::Assistant,
            content: calls,
        }
    }

    /// A tool-result turn for a single call.
    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error: false,
            }],
        }
    }

    /// All text in the message, joined.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every tool-use block in the message.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

/// A tool the model is allowed to call, in provider-neutral form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub input_schema: Value,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        ToolSpec {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// A model the provider can route to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// Human-readable label shown in the TUI status bar.
    pub display_name: String,
    /// Context window in tokens.
    pub context_window: usize,
    /// Maximum tokens the model may generate in one response.
    pub max_output_tokens: usize,
    /// Whether the model supports parallel tool calls.
    pub supports_parallel_tools: bool,
}

impl ModelInfo {
    pub fn new(id: impl Into<String>, context_window: usize) -> Self {
        let id = id.into();
        ModelInfo {
            display_name: id.clone(),
            id,
            context_window,
            max_output_tokens: 8192,
            supports_parallel_tools: true,
        }
    }
}

/// A completion request.
#[derive(Debug, Clone, Default)]
pub struct CompletionRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: Option<usize>,
    pub temperature: Option<f64>,
    /// Provider-specific passthrough (e.g. Anthropic `thinking` config).
    pub extra: Option<Value>,
}

/// A completed, non-streaming response.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResponse {
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<Usage>,
}

impl CompletionResponse {
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
}

/// Token accounting for a request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// An incremental piece of a streamed response.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A chunk of assistant text.
    Text(String),
    /// A chunk of the model's thinking.
    Thinking(String),
    /// A tool call that has started; `id` identifies the call.
    ToolCallStart { id: String, name: String },
    /// A chunk of JSON arguments for the tool call with the given id.
    ToolCallDelta { id: String, arguments: String },
    /// The tool call is complete.
    ToolCallEnd { id: String },
    /// The model finished; carries the final usage if the provider reports it.
    Done {
        stop_reason: Option<StopReason>,
        usage: Option<Usage>,
    },
    /// The provider sent a keep-alive or opaque event worth surfacing.
    Ping,
}

impl StreamEvent {
    pub fn text(s: impl Into<String>) -> Self {
        StreamEvent::Text(s.into())
    }
}
