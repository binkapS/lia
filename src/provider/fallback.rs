//! A provider that tries another model when one cannot answer.
//!
//! Models go down, run out of credit, and change their names. A single
//! model is a single point of failure: when it stops answering, the
//! assistant stops answering. This provider asks the next one instead.
//!
//! Only failures that another model might survive are retried. A request
//! the provider rejected as invalid, or a response it could not read, will
//! be refused the same way by the next model, so there is nothing to gain
//! by asking it.

use std::sync::Arc;

use async_trait::async_trait;

use super::types::{CompletionRequest, CompletionResponse, ModelInfo};
use super::{EventStream, Provider, ProviderError, Result};

/// A provider that falls back to other models.
pub struct FallbackProvider {
    /// The models to ask, in order.
    providers: Vec<Arc<dyn Provider>>,
}

impl FallbackProvider {
    /// A provider that asks the given models in order.
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> FallbackProvider {
        FallbackProvider { providers }
    }

    /// The models this provider asks.
    pub fn providers(&self) -> &[Arc<dyn Provider>] {
        &self.providers
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    fn name(&self) -> &str {
        self.providers
            .first()
            .map(|provider| provider.name())
            .unwrap_or("fallback")
    }

    fn model(&self) -> &str {
        self.providers
            .first()
            .map(|provider| provider.model())
            .unwrap_or("")
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let mut last: Option<ProviderError> = None;

        for provider in &self.providers {
            let mut attempt = request.clone();
            attempt.model = provider.model().to_string();

            match provider.complete(&attempt).await {
                Ok(response) => return Ok(response),
                Err(error) if error.is_retryable() => {
                    tracing::warn!(
                        "the model '{}' could not answer ({error}); trying the next one",
                        provider.model()
                    );
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }

        Err(last.unwrap_or_else(|| ProviderError::Config("no model is configured".into())))
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        let mut last: Option<ProviderError> = None;

        for provider in &self.providers {
            let mut attempt = request.clone();
            attempt.model = provider.model().to_string();

            match provider.stream(&attempt).await {
                Ok(stream) => return Ok(stream),
                Err(error) if error.is_retryable() => {
                    tracing::warn!(
                        "the model '{}' could not answer ({error}); trying the next one",
                        provider.model()
                    );
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }

        Err(last.unwrap_or_else(|| ProviderError::Config("no model is configured".into())))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        match self.providers.first() {
            Some(provider) => provider.list_models().await,
            None => Ok(Vec::new()),
        }
    }
}
