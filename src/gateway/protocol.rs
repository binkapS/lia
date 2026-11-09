//! Wire types for the gateway protocol.
//!
//! These are the shapes the HTTP API and the WebSocket stream exchange. They
//! are deliberately separate from the internal types so the wire format can
//! evolve without churning the agent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::AgentEvent;
use crate::provider::types::Usage;

/// Response to `GET /health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub model: String,
    pub provider: String,
    pub sessions: usize,
    /// Whether this gateway can synthesize speech at `/v1/audio/speech`.
    ///
    /// The browser asks before it starts speaking, so it can use the same
    /// voice as the terminal instead of the one built into the browser.
    #[serde(default)]
    pub speech: bool,
}

/// The gateway's own half of a system reading.
///
/// The monitor in the browser shows the machine and the program that is
/// answering through it; this is the second half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayStatus {
    pub host: String,
    pub port: u16,
    /// Whether the gateway has a token.
    pub secure: bool,
    pub approval: String,
    pub model: String,
    pub provider: String,
    pub sessions: usize,
    pub tools: usize,
    pub skills: usize,
}

/// A reading of the machine and the gateway, for the system monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub system: crate::system::SystemSnapshot,
    pub gateway: GatewayStatus,
}

/// A session, in list form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
}

/// A picture sent to the gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePayload {
    /// The kind of picture, e.g. `image/png`.
    pub media_type: String,
    /// The picture itself, base64.
    pub data: String,
}

impl ImagePayload {
    /// The picture as the message carries it.
    pub fn to_block(&self) -> crate::provider::types::ContentBlock {
        crate::provider::types::ContentBlock::Image {
            media_type: self.media_type.clone(),
            data: self.data.clone(),
        }
    }
}

/// A message sent to the gateway over HTTP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageRequest {
    /// The user's message.
    pub content: String,
    /// Whether to answer without using tools.
    #[serde(default)]
    pub no_tools: bool,
    /// Pictures sent along with the message.
    #[serde(default)]
    pub images: Vec<ImagePayload>,
}

/// A completed turn, returned by the HTTP endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageResponse {
    pub session_id: String,
    /// The assistant's reply text.
    pub reply: String,
    /// Every event emitted during the turn, in order.
    pub events: Vec<WireEvent>,
    pub usage: Option<Usage>,
    pub iterations: usize,
}

/// A frame sent by a client over the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Send a user message and start a turn.
    Send {
        content: String,
        #[serde(default)]
        session_id: Option<String>,
        /// Pictures sent along with the message.
        #[serde(default)]
        images: Vec<ImagePayload>,
    },
    /// Approve a pending tool call.
    Approve {
        request_id: String,
        #[serde(default)]
        always: bool,
    },
    /// Deny a pending tool call.
    Deny { request_id: String },
    /// Interrupt a running turn.
    Cancel { session_id: String },
    /// Subscribe to a session's events without sending a message.
    Subscribe { session_id: String },
    /// List the open sessions.
    ListSessions,
    /// Create a session.
    CreateSession,
    /// Ask what the gateway is.
    Health,
    /// Ask the gateway to shut down.
    Shutdown,
    /// A request in the request/response envelope.
    ///
    /// The envelope is for clients that want one answer per request and an
    /// id to match it by: the gateway answers with a `res` frame carrying
    /// the same id, and pushes the turn's progress as `event` frames.
    #[serde(rename = "req")]
    Request {
        id: String,
        method: String,
        #[serde(default)]
        params: Value,
    },
}

/// A frame sent by the gateway over the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The gateway is ready.
    Hello { version: String, model: String },
    /// An agent event, forwarded from the running turn.
    Event { event: WireEvent },
    /// A tool call needs approval.
    ApprovalRequest {
        request_id: String,
        tool: String,
        summary: String,
        arguments: String,
    },
    /// The list of sessions.
    Sessions { sessions: Vec<SessionSummary> },
    /// A session was created.
    SessionCreated { session: SessionSummary },
    /// What the gateway is.
    Health { health: HealthResponse },
    /// The turn finished.
    Done {
        session_id: String,
        reply: String,
        usage: Option<Usage>,
        iterations: usize,
    },
    /// Something went wrong.
    Error { message: String },
}

/// The answer to a request.
///
/// It carries the id of the request it answers, so a client with several
/// requests in flight can tell them apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFrame {
    /// Always `res`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The id of the request this answers.
    pub id: String,
    /// Whether the request worked.
    pub ok: bool,
    /// The result, when it worked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Why it failed, when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ResponseFrame {
    /// A successful answer.
    pub fn ok(id: impl Into<String>, data: Value) -> Self {
        ResponseFrame {
            kind: "res".to_string(),
            id: id.into(),
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    /// A failed answer.
    pub fn failed(id: impl Into<String>, error: impl Into<String>) -> Self {
        ResponseFrame {
            kind: "res".to_string(),
            id: id.into(),
            ok: false,
            data: None,
            error: Some(error.into()),
        }
    }
}

/// Something that happened, pushed to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFrame {
    /// Always `event`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The name of the event, e.g. `text_delta`.
    pub event: String,
    /// What happened.
    pub data: Value,
}

impl EventFrame {
    /// An event with a name and a body.
    pub fn new(event: impl Into<String>, data: Value) -> Self {
        EventFrame {
            kind: "event".to_string(),
            event: event.into(),
            data,
        }
    }
}

/// A frame on the wire.
///
/// The three kinds are told apart by their `type` field: the original
/// protocol's messages, the answers to requests, and events.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum OutFrame {
    /// One of the original protocol's messages.
    Message(ServerMessage),
    /// The answer to a request.
    Response(ResponseFrame),
    /// Something that happened.
    Event(EventFrame),
}

/// The arguments of a request.
///
/// Both spellings of a name are accepted — `sessionId` and `session_id` —
/// so a client written against either convention works.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestParams {
    /// The conversation to talk in.
    #[serde(default, alias = "session_id")]
    pub session_id: Option<String>,
    /// The message to send.
    #[serde(default)]
    pub content: Option<String>,
    /// Pictures sent along with the message.
    #[serde(default)]
    pub images: Option<Vec<ImagePayload>>,
    /// The tool call to answer.
    #[serde(default, alias = "request_id")]
    pub request_id: Option<String>,
    /// Whether the answer applies to every later call of that tool.
    #[serde(default)]
    pub always: Option<bool>,
}

/// A serializable view of an [`AgentEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WireEvent {
    Started {
        model: String,
    },
    TextDelta {
        text: String,
    },
    /// The model thinking out loud.
    ThinkingDelta {
        text: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
        input: Value,
    },
    ToolCallFinished {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    TurnComplete {
        text: String,
        usage: Option<Usage>,
    },
    Finished {
        usage: Option<Usage>,
        iterations: usize,
    },
    /// What another assistant is doing, in the conversation that handed it
    /// its work.
    SubAssistant {
        label: String,
        note: String,
    },
    Notice {
        message: String,
    },
    Error {
        message: String,
    },
}

/// Split an event into its name and its body.
///
/// The wire form of an event is `{"event":"text_delta","text":"..."}`; the
/// envelope carries the name in `event` and the rest of it as the body.
pub fn split_event(event: WireEvent) -> (String, Value) {
    let mut value = serde_json::to_value(&event).unwrap_or(Value::Null);
    let name = value
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("event")
        .to_string();
    if let Some(map) = value.as_object_mut() {
        map.remove("event");
    }
    (name, value)
}

impl From<AgentEvent> for WireEvent {
    fn from(event: AgentEvent) -> Self {
        match event {
            AgentEvent::Started { model } => WireEvent::Started { model },
            AgentEvent::TextDelta(text) => WireEvent::TextDelta { text },
            AgentEvent::ThinkingDelta(text) => WireEvent::ThinkingDelta { text },
            AgentEvent::ToolCallStarted { id, name, input } => {
                WireEvent::ToolCallStarted { id, name, input }
            }
            AgentEvent::ToolCallFinished { id, name, output } => WireEvent::ToolCallFinished {
                id,
                name,
                content: output.content,
                is_error: output.is_error,
            },
            AgentEvent::TurnComplete { text, usage, .. } => WireEvent::TurnComplete { text, usage },
            AgentEvent::Finished {
                usage, iterations, ..
            } => WireEvent::Finished { usage, iterations },
            AgentEvent::SubAssistant { label, note } => WireEvent::SubAssistant { label, note },
            AgentEvent::Notice(message) => WireEvent::Notice { message },
            AgentEvent::Error(message) => WireEvent::Error { message },
        }
    }
}

/// An error body returned by the HTTP API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

impl ErrorResponse {
    pub fn new(message: impl Into<String>) -> Self {
        ErrorResponse {
            error: message.into(),
        }
    }
}
