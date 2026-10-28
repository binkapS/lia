//! Tests of trying another model when one cannot answer.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lia::provider::fallback::FallbackProvider;
use lia::provider::types::{
    CompletionRequest, CompletionResponse, ContentBlock, StopReason, StreamEvent,
};
use lia::provider::{EventStream, Provider, ProviderError, Result};

/// How a model refuses.
#[derive(Debug, Clone)]
enum Refusal {
    /// The provider is busy.
    Busy(String),
    /// The model is not there.
    Gone(String),
    /// The server is in trouble.
    Broken(String),
    /// The credentials were refused.
    Refused(String),
}

impl Refusal {
    /// The error this refusal is.
    fn error(&self) -> ProviderError {
        match self {
            Refusal::Busy(message) => ProviderError::RateLimited(message.clone()),
            Refusal::Gone(message) => ProviderError::ModelNotFound(message.clone()),
            Refusal::Broken(message) => ProviderError::Http {
                status: 503,
                body: message.clone(),
            },
            Refusal::Refused(message) => ProviderError::Auth(message.clone()),
        }
    }
}

/// A model that either answers or refuses.
struct Stub {
    model: &'static str,
    /// How it refuses, if it refuses.
    error: Option<Refusal>,
    /// The models it was asked for.
    asked: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn new(model: &'static str, error: Option<Refusal>) -> (Arc<Stub>, Arc<Mutex<Vec<String>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let stub = Arc::new(Stub {
            model,
            error,
            asked: asked.clone(),
        });
        (stub, asked)
    }
}

#[async_trait]
impl Provider for Stub {
    fn name(&self) -> &str {
        "stub"
    }

    fn model(&self) -> &str {
        self.model
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.asked.lock().unwrap().push(request.model.clone());
        if let Some(error) = &self.error {
            return Err(error.error());
        }
        Ok(CompletionResponse {
            content: vec![ContentBlock::text(format!("{} answered", self.model))],
            model: self.model.to_string(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        self.asked.lock().unwrap().push(request.model.clone());
        if let Some(error) = &self.error {
            return Err(error.error());
        }
        let text = format!("{} answered", self.model);
        let stream = async_stream::stream! {
            yield Ok(StreamEvent::text(text));
            yield Ok(StreamEvent::Done {
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            });
        };
        Ok(Box::pin(stream))
    }
}

/// A request that says nothing.
fn request() -> CompletionRequest {
    CompletionRequest {
        model: "the-first-one".into(),
        system: None,
        messages: vec![],
        tools: vec![],
        max_output_tokens: Some(100),
        temperature: None,
        extra: None,
    }
}

#[tokio::test]
async fn a_model_that_answers_is_asked_first() {
    let (first, asked) = Stub::new("first", None);
    let (second, _) = Stub::new("second", None);

    let provider = FallbackProvider::new(vec![first.clone(), second.clone()]);
    let response = provider.complete(&request()).await.expect("an answer");

    assert_eq!(response.text(), "first answered");
    assert_eq!(asked.lock().unwrap().len(), 1, "the second model was asked");
}

#[tokio::test]
async fn a_model_that_cannot_answer_gives_way_to_the_next() {
    let (first, _) = Stub::new("first", Some(Refusal::Busy("busy".into())));
    let (second, asked) = Stub::new("second", None);

    let provider = FallbackProvider::new(vec![first.clone(), second.clone()]);
    let response = provider.complete(&request()).await.expect("an answer");

    assert_eq!(response.text(), "second answered");
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn every_model_is_tried_in_turn() {
    let (first, _) = Stub::new("first", Some(Refusal::Busy("busy".into())));
    let (second, _) = Stub::new("second", Some(Refusal::Gone("gone".into())));
    let (third, asked) = Stub::new("third", None);

    let provider = FallbackProvider::new(vec![first.clone(), second.clone(), third.clone()]);
    let response = provider.complete(&request()).await.expect("an answer");

    assert_eq!(response.text(), "third answered");
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_server_in_trouble_gives_way_to_the_next() {
    let (first, _) = Stub::new("first", Some(Refusal::Broken("unavailable".into())));
    let (second, asked) = Stub::new("second", None);

    let provider = FallbackProvider::new(vec![first.clone(), second.clone()]);
    let response = provider.complete(&request()).await.expect("an answer");

    assert_eq!(response.text(), "second answered");
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_request_that_was_refused_is_not_asked_again() {
    // A request the provider rejected as invalid will be rejected the same
    // way by every model, so there is nothing to gain by asking again.
    let (first, _) = Stub::new("first", Some(Refusal::Refused("no key".into())));
    let (second, asked) = Stub::new("second", None);

    let provider = FallbackProvider::new(vec![first.clone(), second.clone()]);
    let error = provider.complete(&request()).await.unwrap_err();

    assert!(matches!(error, ProviderError::Auth(_)));
    assert!(
        asked.lock().unwrap().is_empty(),
        "the second model was asked"
    );
}

#[tokio::test]
async fn when_every_model_refuses_the_last_word_is_the_error() {
    let (first, _) = Stub::new("first", Some(Refusal::Busy("first".into())));
    let (second, _) = Stub::new("second", Some(Refusal::Busy("second".into())));

    let provider = FallbackProvider::new(vec![first.clone(), second.clone()]);
    let error = provider.complete(&request()).await.unwrap_err();

    assert!(
        error.to_string().contains("second"),
        "the error should be the last one: {error}"
    );
}

#[tokio::test]
async fn a_stream_falls_back_too() {
    let (first, _) = Stub::new("first", Some(Refusal::Busy("busy".into())));
    let (second, _) = Stub::new("second", None);

    let provider = FallbackProvider::new(vec![first.clone(), second.clone()]);
    let stream = provider.stream(&request()).await.expect("a stream");

    let events: Vec<_> = futures_util::StreamExt::collect::<Vec<_>>(stream).await;
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Ok(StreamEvent::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "second answered");
}

#[tokio::test]
async fn each_model_is_asked_for_its_own_name() {
    let (first, first_asked) = Stub::new("first", Some(Refusal::Busy("busy".into())));
    let (second, second_asked) = Stub::new("second", None);

    let provider = FallbackProvider::new(vec![first.clone(), second.clone()]);
    provider.complete(&request()).await.expect("an answer");

    assert_eq!(first_asked.lock().unwrap()[0], "first");
    assert_eq!(second_asked.lock().unwrap()[0], "second");
}

#[test]
fn a_config_can_name_the_models_to_try_in_turn() {
    let text = r#"
provider = "openai"
model = "gpt-4o-mini"

[[fallback]]
provider = "anthropic"
model = "claude-sonnet-4-5"

[[fallback]]
provider = "openai"
model = "llama-3.1-70b"
base_url = "http://localhost:11434/v1"
api_key_env = "OLLAMA_API_KEY"
"#;

    let config: lia::Config = toml::from_str(text).expect("the config reads");
    assert_eq!(config.fallback.len(), 2);
    assert_eq!(config.fallback[0].model, "claude-sonnet-4-5");
    assert_eq!(
        config.fallback[1].base_url.as_deref(),
        Some("http://localhost:11434/v1")
    );
    assert_eq!(
        config.fallback[1].api_key_env.as_deref(),
        Some("OLLAMA_API_KEY")
    );
}

#[test]
fn a_config_without_fallbacks_has_none() {
    let config: lia::Config = toml::from_str("provider = \"openai\"").expect("the config reads");
    assert!(config.fallback.is_empty());
}
