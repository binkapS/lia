//! The gateway: a local daemon that owns the agent and serves clients.
//!
//! The TUI, the CLI, and messaging channels all talk to the same agent
//! through this service, so a conversation started in one place can be
//! continued in another.
//!
//! ## Protocol
//!
//! HTTP for request/response and WebSocket for live streaming:
//!
//! | Route | Purpose |
//! |---|---|
//! | `GET  /health` | liveness and version |
//! | `GET  /v1/system` | the machine and gateway reading for the monitor |
//! | `GET  /` | the interface, in a browser |
//! | `GET  /v1/models` | models the configured provider offers |
//! | `GET  /v1/tools` | tools the assistant can call |
//! | `GET  /v1/skills` | skills that were found |
//! | `GET  /v1/audio/voices` | voices the built-in speech model offers |
//! | `POST /v1/audio/speech` | synthesize speech in the configured voice |
//! | `GET  /v1/sessions` | list sessions |
//! | `POST /v1/sessions` | create a session |
//! | `GET  /v1/sessions/{id}` | fetch one session |
//! | `DELETE /v1/sessions/{id}` | delete a session |
//! | `POST /v1/sessions/{id}/messages` | send a message, get the reply |
//! | `GET  /v1/inbox` | list the local user's inbox records |
//! | `POST /v1/inbox` | capture a record |
//! | `GET  /v1/inbox/{id}` | fetch one inbox record |
//! | `PATCH /v1/inbox/{id}` | update state or text |
//! | `POST /v1/inbox/{id}/defer` | defer an inbox record |
//! | `DELETE /v1/inbox/{id}` | delete an inbox record |
//! | `WS   /v1/ws` | send messages, receive streaming events |
//!
//! The WebSocket protocol is newline-delimited JSON: each frame is one
//! `{"type": ..., ...}` object. A client may use either of two shapes:
//!
//! | Frame | Meaning |
//! |---|---|
//! | `{"type":"send","content":"..."}` | the original protocol: a frame in, a frame out |
//! | `{"type":"req","id":"1","method":"message.send","params":{...}}` | the envelope: one answer per request, matched by id |
//!
//! In the envelope the gateway answers with `{"type":"res","id":"1","ok":true,
//! "data":{...}}` and pushes what happens meanwhile as `{"type":"event",
//! "event":"text_delta","data":{...}}` frames.

pub mod protocol;
pub mod server;
pub mod web;

pub use protocol::{
    ClientMessage, ErrorResponse, EventFrame, GatewayStatus, HealthResponse, OutFrame,
    RequestParams, ResponseFrame, ServerMessage, SessionSummary, SystemStatus, WireEvent,
    split_event,
};
pub use server::{Gateway, GatewayHandle};
