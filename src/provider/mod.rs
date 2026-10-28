//! Provider abstraction: one trait, many backends.
//!
//! A [`Provider`] turns a [`CompletionRequest`] into either a full
//! [`CompletionResponse`] or a stream of [`StreamEvent`]s. Backends translate
//! to and from the neutral types in [`types`].

pub mod anthropic;
pub mod fallback;
pub mod ollama;
pub mod openai_compat;
pub mod retry;
pub mod types;

use std::pin::Pin;

use async_trait::async_trait;
use futures_util::Stream;
use thiserror::Error;

pub use types::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ModelInfo, Role, StopReason,
    StreamEvent, ToolSpec, Usage,
};

/// Errors a provider can surface.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The HTTP request itself failed.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// The provider returned a non-success status.
    #[error("provider returned {status}: {body}")]
    Http { status: u16, body: String },

    /// The response body did not match the expected schema.
    #[error("malformed response: {0}")]
    Decode(String),

    /// The provider rejected the request as invalid.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The model does not exist or the caller may not use it.
    #[error("model not available: {0}")]
    ModelNotFound(String),

    /// The provider is rate limiting us.
    #[error("rate limited: {0}")]
    RateLimited(String),

    /// The provider ran out of context space.
    #[error("context window exceeded: {0}")]
    ContextOverflow(String),

    /// Configuration is missing or malformed.
    #[error("configuration error: {0}")]
    Config(String),

    /// Credentials are missing or rejected.
    #[error("authentication failed: {0}")]
    Auth(String),
}

impl ProviderError {
    /// Whether retrying the same request could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::Network(_) | ProviderError::RateLimited(_) => true,
            // Another model may well be there.
            ProviderError::ModelNotFound(_) => true,
            ProviderError::Http { status, .. } => {
                *status == 429
                    || *status == 500
                    || *status == 502
                    || *status == 503
                    || *status == 504
            }
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, ProviderError>;

/// The value of an authorization header for a key.
///
/// A key that already carries the word is left as it is; one that does not
/// is given it, which is what the servers expect.
pub fn bearer(key: &str) -> String {
    if key.trim_start().to_ascii_lowercase().starts_with("bearer ") {
        key.trim_start().to_string()
    } else {
        format!("Bearer {key}")
    }
}

/// A stream of provider events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

/// A model backend.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable identifier for this backend, e.g. `anthropic`, `openai`.
    fn name(&self) -> &str;

    /// The model this provider was configured with.
    fn model(&self) -> &str;

    /// Complete a request, returning the whole response at once.
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse>;

    /// Complete a request, streaming events as they arrive.
    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream>;

    /// List the models this provider can serve, if it supports discovery.
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }

    /// Rough token count for a request, used to decide when to compact.
    fn estimate_tokens(&self, request: &CompletionRequest) -> usize {
        estimate_request_tokens(request)
    }
}

/// Approximate token count when a provider offers no tokenizer.
///
/// Uses ~4 characters per token, which is close enough for the purpose of
/// deciding whether to compact a conversation.
pub fn estimate_request_tokens(request: &CompletionRequest) -> usize {
    let mut chars = 0usize;
    for message in &request.messages {
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => chars += text.len(),
                ContentBlock::Thinking { text } => chars += text.len(),
                ContentBlock::ToolUse { name, input, .. } => {
                    chars += name.len() + input.to_string().len();
                }
                ContentBlock::ToolResult { content, .. } => chars += content.len(),
                ContentBlock::Image { .. } => chars += 1_600, // rough cost of an image
            }
        }
        chars += 4; // role/framing overhead
    }
    if let Some(system) = &request.system {
        chars += system.len();
    }
    chars / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_given_the_word_the_servers_expect() {
        assert_eq!(bearer("sk-abc"), "Bearer sk-abc");
    }

    #[test]
    fn a_key_that_already_has_the_word_is_left_alone() {
        assert_eq!(bearer("Bearer sk-abc"), "Bearer sk-abc");
        assert_eq!(bearer("bearer sk-abc"), "bearer sk-abc");
    }
}
