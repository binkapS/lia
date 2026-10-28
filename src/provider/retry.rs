//! Trying again when the provider will not answer.
//!
//! A model backend fails for reasons that pass: the network hiccups, the
//! server is busy for a moment, the rate limit resets. Asking again, after
//! a pause, is often all that is needed — and much better than telling the
//! person that the assistant is down.
//!
//! Only failures that could go away are tried again. A request the
//! provider refused as wrong is not asked a second time: it would be
//! refused the same way.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::types::{CompletionRequest, CompletionResponse, ModelInfo};
use super::{EventStream, Provider, Result};

/// How a failing provider is asked again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryConfig {
    /// How many times a request is sent in all. One means no retry.
    pub attempts: u32,
    /// How long the first pause is, in milliseconds. The later pauses are
    /// longer.
    pub backoff_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        RetryConfig {
            attempts: 3,
            backoff_ms: 500,
        }
    }
}

impl RetryConfig {
    /// The pause before the first try again.
    pub fn backoff(&self) -> Duration {
        Duration::from_millis(self.backoff_ms)
    }
}

/// A provider that asks again when the answer is worth waiting for.
pub struct RetryProvider {
    /// The provider underneath.
    inner: Arc<dyn Provider>,
    /// How many times the request is sent in all.
    attempts: u32,
    /// How long the first pause is. The next ones are longer.
    backoff: Duration,
}

impl RetryProvider {
    /// A provider that tries `attempts` times, waiting `backoff` between
    /// them.
    pub fn new(inner: Arc<dyn Provider>, attempts: u32, backoff: Duration) -> RetryProvider {
        RetryProvider {
            inner,
            attempts: attempts.max(1),
            backoff,
        }
    }

    /// A provider that tries again as the settings say.
    pub fn with_config(inner: Arc<dyn Provider>, config: &RetryConfig) -> RetryProvider {
        RetryProvider::new(inner, config.attempts, config.backoff())
    }

    /// How long to wait before the given try.
    ///
    /// The pause grows with the number of tries, so a provider that is
    /// down for a while is not hammered.
    fn pause(&self, attempt: u32) -> Duration {
        self.backoff * (1 << attempt.min(4))
    }
}

#[async_trait]
impl Provider for RetryProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let mut attempt = 0;
        loop {
            match self.inner.complete(request).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if attempt + 1 >= self.attempts || !e.is_retryable() {
                        return Err(e);
                    }
                    tracing::warn!("the provider could not answer ({e}), trying again in a moment");
                    tokio::time::sleep(self.pause(attempt)).await;
                    attempt += 1;
                }
            }
        }
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        let mut attempt = 0;
        loop {
            match self.inner.stream(request).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    if attempt + 1 >= self.attempts || !e.is_retryable() {
                        return Err(e);
                    }
                    tracing::warn!("the provider could not answer ({e}), trying again in a moment");
                    tokio::time::sleep(self.pause(attempt)).await;
                    attempt += 1;
                }
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        self.inner.list_models().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderError;
    use crate::provider::types::ContentBlock;

    /// A provider that fails a number of times, then answers.
    struct FlakyProvider {
        /// How many times it should fail before answering.
        failures: std::sync::Mutex<u32>,
        /// Whether the failure is one worth trying again.
        retryable: bool,
        /// How many times it was asked.
        asked: std::sync::Mutex<u32>,
    }

    impl FlakyProvider {
        /// A provider that fails `failures` times and then answers.
        fn new(failures: u32) -> Arc<FlakyProvider> {
            Arc::new(FlakyProvider {
                failures: std::sync::Mutex::new(failures),
                retryable: true,
                asked: std::sync::Mutex::new(0),
            })
        }

        /// A provider whose failures are never worth trying again.
        fn refusing(failures: u32) -> Arc<FlakyProvider> {
            Arc::new(FlakyProvider {
                failures: std::sync::Mutex::new(failures),
                retryable: false,
                asked: std::sync::Mutex::new(0),
            })
        }

        /// How many times it was asked.
        fn asked(&self) -> u32 {
            *self.asked.lock().unwrap()
        }
    }

    #[async_trait]
    impl Provider for FlakyProvider {
        fn name(&self) -> &str {
            "flaky"
        }

        fn model(&self) -> &str {
            "flaky-model"
        }

        async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
            *self.asked.lock().unwrap() += 1;
            let mut failures = self.failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(if self.retryable {
                    ProviderError::RateLimited("slow down".into())
                } else {
                    ProviderError::InvalidRequest("no".into())
                });
            }
            Ok(CompletionResponse {
                content: vec![ContentBlock::text("the answer")],
                model: "flaky-model".into(),
                stop_reason: None,
                usage: None,
            })
        }

        async fn stream(&self, _request: &CompletionRequest) -> Result<EventStream> {
            Err(ProviderError::RateLimited("slow down".into()))
        }
    }

    /// A request, of no particular interest.
    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "flaky-model".into(),
            system: None,
            messages: vec![crate::provider::types::Message::user("hello")],
            tools: Vec::new(),
            max_output_tokens: None,
            temperature: None,
            extra: None,
        }
    }

    #[tokio::test]
    async fn a_hiccup_is_tried_again() {
        let inner = FlakyProvider::new(2);
        let provider = RetryProvider::new(inner.clone(), 3, Duration::from_millis(1));

        let response = provider.complete(&request()).await.expect("it answers");
        assert_eq!(response.text(), "the answer");
        assert_eq!(inner.asked(), 3, "it was not asked three times");
    }

    #[tokio::test]
    async fn a_refusal_is_not_tried_again() {
        let inner = FlakyProvider::refusing(2);
        let provider = RetryProvider::new(inner.clone(), 3, Duration::from_millis(1));

        let error = provider.complete(&request()).await.expect_err("it fails");
        assert!(matches!(error, ProviderError::InvalidRequest(_)));
        assert_eq!(inner.asked(), 1, "it was asked again");
    }

    #[tokio::test]
    async fn a_provider_that_never_answers_gives_up() {
        let inner = FlakyProvider::new(10);
        let provider = RetryProvider::new(inner.clone(), 3, Duration::from_millis(1));

        let error = provider.complete(&request()).await.expect_err("it fails");
        assert!(matches!(error, ProviderError::RateLimited(_)));
        assert_eq!(inner.asked(), 3, "it was asked too often");
    }

    #[tokio::test]
    async fn one_try_means_no_retry() {
        let inner = FlakyProvider::new(10);
        let provider = RetryProvider::new(inner.clone(), 1, Duration::from_millis(1));

        provider.complete(&request()).await.expect_err("it fails");
        assert_eq!(inner.asked(), 1, "it was asked again");
    }

    #[tokio::test]
    async fn the_pause_grows_with_each_try() {
        let provider = RetryProvider::new(FlakyProvider::new(0), 5, Duration::from_millis(100));
        assert_eq!(provider.pause(0), Duration::from_millis(100));
        assert_eq!(provider.pause(1), Duration::from_millis(200));
        assert_eq!(provider.pause(2), Duration::from_millis(400));
        // The growth is capped, so the wait does not run away.
        assert_eq!(provider.pause(9), Duration::from_millis(1600));
    }
}
