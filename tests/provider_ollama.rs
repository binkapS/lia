//! Tests of the Ollama provider.
//!
//! Ollama is spoken to over HTTP, so these run against a small server that
//! answers like one does: the request is looked at, and the reply is read
//! back into the neutral types.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use lia::provider::Provider;
use lia::provider::ollama::OllamaProvider;
use lia::provider::types::{CompletionRequest, ContentBlock, Message, ToolSpec};
use serde_json::{Value, json};

/// A server that answers like Ollama does.
#[derive(Clone)]
struct Fake {
    /// The requests that arrived.
    seen: Arc<Mutex<Vec<Value>>>,
    /// The headers of the last request.
    headers: Arc<Mutex<Option<HeaderMap>>>,
    /// What a plain request is answered with.
    answer: Value,
    /// What a streaming request is answered with, line by line.
    lines: Vec<String>,
}

impl Fake {
    /// A server that answers with the given reply.
    fn answering(answer: Value) -> Arc<Fake> {
        Arc::new(Fake {
            seen: Arc::new(Mutex::new(Vec::new())),
            headers: Arc::new(Mutex::new(None)),
            answer,
            lines: Vec::new(),
        })
    }

    /// A server that streams the given lines.
    fn streaming(lines: Vec<&str>) -> Arc<Fake> {
        Arc::new(Fake {
            seen: Arc::new(Mutex::new(Vec::new())),
            headers: Arc::new(Mutex::new(None)),
            answer: Value::Null,
            lines: lines.into_iter().map(String::from).collect(),
        })
    }

    /// The requests that arrived.
    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }

    /// The last request that arrived.
    fn last_request(&self) -> Value {
        self.requests().last().cloned().unwrap_or(Value::Null)
    }
}

/// The server, and the address it answers on.
async fn serve(fake: Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let app = Router::new()
        .route("/api/chat", post(chat))
        .route("/api/tags", get(tags))
        .with_state((*fake).clone());

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{address}")
}

/// Answer a chat request.
async fn chat(State(fake): State<Fake>, headers: HeaderMap, body: Bytes) -> String {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    fake.seen.lock().unwrap().push(body.clone());
    *fake.headers.lock().unwrap() = Some(headers);

    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        return fake.lines.join("\n");
    }
    fake.answer.to_string()
}

/// List the models.
async fn tags(State(_fake): State<Fake>) -> String {
    json!({
        "models": [
            { "name": "llama3.2", "model": "llama3.2", "size": 2019393189 },
            { "name": "qwen3:4b", "model": "qwen3:4b", "size": 2600000000u64 },
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

/// A tool the model may call.
fn a_tool() -> ToolSpec {
    ToolSpec::new(
        "read_file",
        "Read a file",
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    )
}

#[tokio::test]
async fn a_request_carries_the_model_the_messages_and_the_tools() {
    let fake = Fake::answering(json!({
        "model": "llama3.2",
        "message": { "role": "assistant", "content": "hi there" },
        "done": true,
        "done_reason": "stop",
    }));
    let address = serve(fake.clone()).await;

    let mut request = request("llama3.2");
    request.tools = vec![a_tool()];
    request.messages.push(Message::user_with_images(
        "what is this?",
        vec![ContentBlock::Image {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }],
    ));

    let provider = OllamaProvider::new(None, "llama3.2").with_base_url(&address);
    let response = provider.complete(&request).await.expect("it answers");

    assert_eq!(response.text(), "hi there");

    let body = fake.last_request();
    assert_eq!(body["model"], "llama3.2");
    assert_eq!(body["options"]["num_predict"], 256);
    assert_eq!(body["options"]["temperature"], 0.5);

    let messages = body["messages"].as_array().expect("the messages");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "you are lia");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "hello");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["images"][0], "aGVsbG8=");

    let tools = body["tools"].as_array().expect("the tools");
    assert_eq!(tools[0]["function"]["name"], "read_file");
    assert_eq!(tools[0]["function"]["parameters"]["required"][0], "path");
}

#[tokio::test]
async fn the_thinking_is_kept_apart_from_the_answer() {
    let fake = Fake::answering(json!({
        "model": "gpt-oss:120b",
        "message": {
            "role": "assistant",
            "thinking": "the user says hello",
            "content": "hello to you too",
        },
        "done": true,
        "done_reason": "stop",
        "prompt_eval_count": 12,
        "eval_count": 7,
    }));
    let address = serve(fake).await;

    let provider = OllamaProvider::new(None, "gpt-oss:120b").with_base_url(&address);
    let response = provider
        .complete(&request("gpt-oss:120b"))
        .await
        .expect("it answers");

    assert_eq!(response.text(), "hello to you too");
    let thinking = response
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Thinking { text } => Some(text.clone()),
            _ => None,
        })
        .expect("the thinking is there");
    assert_eq!(thinking, "the user says hello");

    let usage = response.usage.expect("the tokens are counted");
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 7);
}

#[tokio::test]
async fn a_tool_call_is_read_with_its_arguments() {
    let fake = Fake::answering(json!({
        "model": "llama3.2",
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "type": "function",
                "function": {
                    "index": 0,
                    "name": "read_file",
                    "arguments": { "path": "notes.txt" },
                },
            }],
        },
        "done": true,
        "done_reason": "stop",
    }));
    let address = serve(fake.clone()).await;

    let provider = OllamaProvider::new(None, "llama3.2").with_base_url(&address);
    let response = provider
        .complete(&request("llama3.2"))
        .await
        .expect("it answers");

    let mut calls = response.tool_uses();
    let (id, name, input) = calls.next().expect("the tool call is there");
    let id = id.to_string();
    let name = name.to_string();
    let input = input.clone();
    assert_eq!(name, "read_file");
    assert_eq!(input["path"], "notes.txt");
    assert!(!id.is_empty(), "the call has no id of its own");

    // The result of that call goes back, naming the tool it answers.
    let mut follow_up = request("llama3.2");
    follow_up.messages = vec![
        Message::user("what is in the file?"),
        Message::assistant_tool_use(vec![ContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        }]),
        Message::tool_result(&id, "the contents"),
    ];

    provider
        .complete(&follow_up)
        .await
        .expect("it answers again");

    let body = fake.last_request();
    let messages = body["messages"].as_array().expect("the messages");
    let result = messages
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("the result is there");
    assert_eq!(result["tool_name"], "read_file");
    assert_eq!(result["content"], "the contents");
}

#[tokio::test]
async fn the_stream_arrives_line_by_line() {
    let fake = Fake::streaming(vec![
        r#"{"message":{"role":"assistant","content":"Hel"},"done":false}"#,
        r#"{"message":{"role":"assistant","thinking":"hmm"},"done":false}"#,
        r#"{"message":{"role":"assistant","content":"lo"},"done":false}"#,
        r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":5,"eval_count":3}"#,
    ]);
    let address = serve(fake).await;

    let provider = OllamaProvider::new(None, "llama3.2").with_base_url(&address);
    let mut stream = provider
        .stream(&request("llama3.2"))
        .await
        .expect("the stream starts");

    use futures_util::StreamExt;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut done = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("the stream is well");
        match event {
            lia::provider::types::StreamEvent::Text(chunk) => text.push_str(&chunk),
            lia::provider::types::StreamEvent::Thinking(chunk) => thinking.push_str(&chunk),
            lia::provider::types::StreamEvent::Done { stop_reason, usage } => {
                done = true;
                assert_eq!(stop_reason, Some(lia::provider::types::StopReason::EndTurn));
                let usage = usage.expect("the tokens are counted");
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 3);
            }
            _ => {}
        }
    }

    assert!(done, "the stream never said it was done");
    assert_eq!(text, "Hello");
    assert_eq!(thinking, "hmm");
}

#[tokio::test]
async fn the_models_are_listed() {
    let fake = Fake::answering(json!({}));
    let address = serve(fake).await;

    let provider = OllamaProvider::new(None, "llama3.2").with_base_url(&address);
    let models = provider.list_models().await.expect("the models are listed");

    let names: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(names, vec!["llama3.2", "qwen3:4b"]);
}

#[tokio::test]
async fn the_key_goes_with_the_request() {
    let fake = Fake::answering(json!({
        "model": "gpt-oss:120b",
        "message": { "role": "assistant", "content": "hi" },
        "done": true,
    }));
    let address = serve(fake.clone()).await;

    let provider = OllamaProvider::new(Some("a-key".into()), "gpt-oss:120b")
        .cloud()
        .with_base_url(&address);
    provider
        .complete(&request("gpt-oss:120b"))
        .await
        .expect("it answers");

    let headers = fake.headers.lock().unwrap().clone().expect("the headers");
    let sent = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .expect("the key was sent");
    assert_eq!(sent, "Bearer a-key");
}

#[tokio::test]
async fn a_local_server_is_asked_without_a_key() {
    let fake = Fake::answering(json!({
        "model": "llama3.2",
        "message": { "role": "assistant", "content": "hi" },
        "done": true,
    }));
    let address = serve(fake.clone()).await;

    let provider = OllamaProvider::new(None, "llama3.2").with_base_url(&address);
    provider
        .complete(&request("llama3.2"))
        .await
        .expect("it answers");

    let headers = fake.headers.lock().unwrap().clone().expect("the headers");
    assert!(
        headers.get("authorization").is_none(),
        "a key was sent to a server of its own"
    );
}
