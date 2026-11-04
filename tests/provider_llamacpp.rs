//! Tests of the llama.cpp provider.
//!
//! `llama-server` speaks the OpenAI shapes under `/v1`, so the provider is
//! the OpenAI-compatible one pointed at the server's address. These run
//! against a small server that answers like one does: the request is looked
//! at, and the reply is read back into the neutral types.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use lia::config::{Config, ProviderKind};
use lia::provider::Provider;
use lia::provider::types::{CompletionRequest, Message};
use serde_json::{Value, json};

/// A server that answers like llama.cpp does.
#[derive(Clone)]
struct Fake {
    /// The requests that arrived.
    seen: Arc<Mutex<Vec<Value>>>,
    /// The headers of the last request.
    headers: Arc<Mutex<Option<HeaderMap>>>,
    /// What a plain request is answered with.
    answer: Value,
}

impl Fake {
    /// A server that answers with the given reply.
    fn answering(answer: Value) -> Arc<Fake> {
        Arc::new(Fake {
            seen: Arc::new(Mutex::new(Vec::new())),
            headers: Arc::new(Mutex::new(None)),
            answer,
        })
    }

    /// The last request that arrived.
    fn last_request(&self) -> Value {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// The headers of the last request.
    fn last_headers(&self) -> Option<HeaderMap> {
        self.headers.lock().unwrap().clone()
    }
}

/// The server, and the address it answers on.
///
/// The models' route is served because `lia doctor` asks for the list the
/// way a person asks what could be chosen.
async fn serve(fake: Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/models", get(models))
        .with_state((*fake).clone());

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{address}/v1")
}

/// Answer a chat completion.
async fn chat(State(fake): State<Fake>, headers: HeaderMap, body: Bytes) -> String {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    fake.seen.lock().unwrap().push(body);
    *fake.headers.lock().unwrap() = Some(headers);
    fake.answer.to_string()
}

/// List the models the server carries; the one model it was started with.
async fn models(State(_fake): State<Fake>) -> String {
    json!({
        "object": "list",
        "data": [
            { "id": "Model-Q4_K_M", "object": "model", "owned_by": "not me" },
        ]
    })
    .to_string()
}

/// A request, of no particular interest.
fn request(model: &str) -> CompletionRequest {
    CompletionRequest {
        model: model.into(),
        system: Some("you are lia".into()),
        messages: vec![Message::user("hello")],
        tools: Vec::new(),
        max_output_tokens: Some(256),
        temperature: Some(0.5),
        extra: None,
    }
}

/// The configuration the tests use, pointed at the mock server.
fn config(base_url: String) -> Config {
    let toml = format!("provider = \"llama.cpp\"\nbase_url = \"{base_url}\"");
    toml::from_str(&toml).unwrap()
}

#[tokio::test]
async fn a_keyless_server_is_used_without_a_key() {
    let fake = Fake::answering(json!({
        "id": "cmpl-1",
        "object": "chat.completion",
        "model": "Model-Q4_K_M",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "the answer" },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 7 },
    }));
    let address = serve(fake.clone()).await;

    let provider = config(address).build_provider().unwrap();
    let response = provider.complete(&request("")).await.unwrap();

    assert_eq!(response.model, "Model-Q4_K_M");
    assert_eq!(
        response.stop_reason,
        Some(lia::provider::types::StopReason::EndTurn)
    );
    let usage = response.usage.expect("the tokens were counted");
    assert_eq!(usage.input_tokens, 5);
    assert_eq!(usage.output_tokens, 7);

    assert!(
        !fake
            .last_headers()
            .is_some_and(|headers| headers.contains_key("authorization")),
        "a keyless server was sent a key"
    );
}

#[tokio::test]
async fn an_unnamed_model_leaves_the_choice_to_the_server() {
    let fake = Fake::answering(json!({
        "model": "Model-Q4_K_M",
        "choices": [{
            "message": { "role": "assistant", "content": "hi" },
            "finish_reason": "stop",
        }],
    }));
    let address = serve(fake.clone()).await;

    let provider = config(address).build_provider().unwrap();
    provider.complete(&request("")).await.unwrap();

    let sent = fake.last_request();
    assert!(
        sent.get("model").is_none(),
        "an unnamed model was named anyway: {sent}"
    );
}

#[tokio::test]
async fn a_named_model_is_asked_for_by_name() {
    let fake = Fake::answering(json!({
        "model": "Model-Q4_K_M",
        "choices": [{
            "message": { "role": "assistant", "content": "hi" },
            "finish_reason": "stop",
        }],
    }));
    let address = serve(fake.clone()).await;

    let provider = config(address).build_provider().unwrap();
    provider.complete(&request("Model-Q4_K_M")).await.unwrap();

    assert_eq!(fake.last_request()["model"], "Model-Q4_K_M");
}

#[tokio::test]
async fn the_models_the_server_carries_are_listed() {
    let fake = Fake::answering(Value::Null);
    let address = serve(fake.clone()).await;

    let provider = config(address).build_provider().unwrap();
    let models = provider.list_models().await.unwrap();

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "Model-Q4_K_M");
}

#[test]
fn the_config_names_the_server_and_accepts_the_aliases() {
    let parsed: Config = toml::from_str(r#"provider = "llama.cpp""#).unwrap();
    assert_eq!(parsed.provider, ProviderKind::LlamaCpp);
    assert_eq!(parsed.provider.as_str(), "llama.cpp");
    assert_eq!(
        parsed.provider.default_base_url(),
        Some("http://localhost:8080/v1")
    );
    assert!(
        !parsed.provider.needs_key(),
        "a llama.cpp server usually needs no key"
    );
    // Whatever the server was started with; the app does not guess.
    assert_eq!(parsed.provider.default_model(), "");

    for alias in ["llama-cpp", "llama_cpp", "llamacpp", "llama-server"] {
        let kind: ProviderKind = alias.parse().expect(alias);
        assert_eq!(kind, ProviderKind::LlamaCpp);
    }

    // The name survives a round-trip through the configuration file.
    let written = toml::to_string(&parsed).unwrap();
    let reread: Config = toml::from_str(&written).unwrap();
    assert_eq!(reread.provider, ProviderKind::LlamaCpp);
}
