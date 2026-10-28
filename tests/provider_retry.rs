//! Tests of the provider that asks again.
//!
//! The point of a retry is that a passing failure goes unnoticed, and a
//! lasting one is given up on. These tests use a provider that fails on
//! purpose, and count the times it was asked.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use lia::provider::retry::{RetryConfig, RetryProvider};
use lia::provider::types::{CompletionRequest, CompletionResponse, ContentBlock, Message};
use lia::provider::{EventStream, Provider, ProviderError, Result};

/// A provider that fails a number of times, then answers.
struct FlakyProvider {
    /// How many times it should fail before answering.
    failures: Mutex<u32>,
    /// Whether the failure is one worth trying again.
    retryable: bool,
    /// How many times it was asked.
    asked: Mutex<u32>,
}

impl FlakyProvider {
    /// A provider that fails `failures` times and then answers.
    fn new(failures: u32) -> Arc<FlakyProvider> {
        Arc::new(FlakyProvider {
            failures: Mutex::new(failures),
            retryable: true,
            asked: Mutex::new(0),
        })
    }

    /// A provider whose failures are never worth trying again.
    fn refusing(failures: u32) -> Arc<FlakyProvider> {
        Arc::new(FlakyProvider {
            failures: Mutex::new(failures),
            retryable: false,
            asked: Mutex::new(0),
        })
    }

    /// How many times it was asked.
    fn asked(&self) -> u32 {
        *self.asked.lock().expect("the count is readable")
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
        *self.asked.lock().expect("the count is readable") += 1;
        let mut failures = self.failures.lock().expect("the count is readable");
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
        messages: vec![Message::user("hello")],
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
async fn the_settings_decide_how_many_times() {
    let config = RetryConfig {
        attempts: 4,
        backoff_ms: 1,
    };
    let inner = FlakyProvider::new(10);
    let provider = RetryProvider::with_config(inner.clone(), &config);

    provider.complete(&request()).await.expect_err("it fails");
    assert_eq!(inner.asked(), 4, "the settings were not followed");
}

#[tokio::test]
async fn the_provider_below_is_still_the_one_that_is_asked() {
    let inner = FlakyProvider::new(0);
    let provider = RetryProvider::with_config(
        inner.clone(),
        &RetryConfig {
            attempts: 3,
            backoff_ms: 1,
        },
    );

    assert_eq!(provider.name(), "flaky");
    assert_eq!(provider.model(), "flaky-model");
}
