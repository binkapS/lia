//! lia — a local-first AI assistant.
//!
//! The crate is organised around three layers:
//!
//! - [`provider`] — a neutral message/tool model plus adapters for the
//!   Anthropic Messages API and any OpenAI-compatible endpoint.
//! - [`tool`] — the tools the model can call, and the approval policy that
//!   gates the risky ones.
//! - [`agent`] — the loop that turns a user message into model calls, tool
//!   calls, and a final answer.
//!
//! On top of that sit [`gateway`], a local daemon that exposes the agent over
//! HTTP and WebSocket, and [`channel`], adapters that connect messaging
//! platforms to it. [`schedule`] runs the work the assistant promises to do
//! later. [`tui`] is the terminal client, and [`cli`] is the command-line
//! entry point.

pub mod agent;
pub mod channel;
pub mod cli;
pub mod config;
pub mod gateway;
pub mod image;
pub mod live;
pub mod mcp;
pub mod memory;
pub mod provider;
pub mod schedule;
pub mod secrets;
pub mod setup;
pub mod skill;
pub mod system;
pub mod tool;
pub mod tui;
pub mod voice;

pub use agent::{Agent, AgentConfig, AgentEvent};
pub use config::Config;
pub use provider::Provider;
pub use tool::ToolRegistry;
