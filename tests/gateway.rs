//! Tests of the gateway: the HTTP API and both WebSocket protocols.
//!
//! A mock provider stands in for a model, so the gateway can be started and
//! driven without a network or an API key.

mod common;

use std::sync::Arc;

use lia::agent::session::SessionStore;
use lia::agent::{Agent, AgentConfig};
use lia::config::Config;
use lia::gateway::Gateway;
use lia::tool::ToolRegistry;
use serde_json::{Value, json};

/// A gateway listening on a free port.
struct TestGateway {
    address: std::net::SocketAddr,
    /// The model behind it, so a test can see what it was asked.
    provider: Arc<common::MockProvider>,
    /// A configuration file of its own, so editing settings in a test
    /// never touches the real one.
    config_path: std::path::PathBuf,
    _state_dir: tempfile::TempDir,
}

impl TestGateway {
    /// Start a gateway on a free port.
    async fn start() -> TestGateway {
        TestGateway::start_with(|_| {}).await
    }

    /// Start a gateway on a free port, with a token if one is wanted.
    async fn start_with_token(token: Option<&str>) -> TestGateway {
        TestGateway::start_with(|config| {
            config.gateway.token = token.map(str::to_string);
        })
        .await
    }

    /// Start a gateway on a free port, with the configuration edited.
    async fn start_with(edit: impl FnOnce(&mut Config)) -> TestGateway {
        let provider = Arc::new(common::MockProvider::new());
        let agent = Agent::new(
            provider.clone(),
            Arc::new(ToolRegistry::new()),
            AgentConfig::default(),
        );

        let state_dir = tempfile::tempdir().unwrap();
        let config_path = state_dir.path().join("config.toml");
        let mut config = Config::default();
        config.gateway.host = "127.0.0.1".into();
        config.gateway.port = 0;
        config.state_dir = state_dir.path().to_path_buf();
        config.config_path = Some(config_path.clone());
        edit(&mut config);

        let gateway = Gateway::start(
            Arc::new(agent),
            Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
            Arc::new(config),
        )
        .await
        .expect("the gateway did not start");

        TestGateway {
            address: gateway.addr(),
            provider,
            config_path,
            _state_dir: state_dir,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn ws_url(&self) -> String {
        format!("ws://{}/v1/ws", self.address)
    }
}

#[tokio::test]
async fn the_health_endpoint_answers() {
    let gateway = TestGateway::start().await;

    let body: Value = reqwest::get(gateway.url("/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["status"], "ok");
    assert_eq!(body["provider"], "mock");
    assert!(
        body["speech"].is_boolean(),
        "the health reading does not say whether it can speak: {body}"
    );
}

#[tokio::test]
async fn the_system_endpoint_reports_the_machine_and_gateway() {
    let gateway = TestGateway::start().await;

    let body: Value = reqwest::get(gateway.url("/v1/system"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(body["system"]["cpu"]["cores"].as_u64().unwrap() > 0);
    assert!(body["system"]["memory"]["total_bytes"].as_u64().unwrap() > 0);
    assert!(
        !body["system"]["health"]["checks"]
            .as_array()
            .unwrap()
            .is_empty(),
        "health checks are what the monitor draws"
    );
    assert_eq!(body["gateway"]["provider"], "mock");
    assert_eq!(body["gateway"]["model"], "mock-model");
    assert!(body["gateway"]["tools"].as_u64().is_some());
}

#[tokio::test]
async fn a_message_over_http_is_answered() {
    let gateway = TestGateway::start().await;

    let created: Value = reqwest::Client::new()
        .post(gateway.url("/v1/sessions"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let reply: Value = reqwest::Client::new()
        .post(gateway.url(&format!("/v1/sessions/{id}/messages")))
        .json(&json!({ "content": "hello" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(reply["reply"], "the answer");
    assert_eq!(reply["session_id"], id);
    assert!(reply["events"].as_array().unwrap().len() > 1);
}

#[tokio::test]
async fn a_picture_sent_over_http_reaches_the_model() {
    let gateway = TestGateway::start().await;

    let created: Value = reqwest::Client::new()
        .post(gateway.url("/v1/sessions"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let reply: Value = reqwest::Client::new()
        .post(gateway.url(&format!("/v1/sessions/{id}/messages")))
        .json(&json!({
            "content": "what is this?",
            "images": [{"media_type": "image/png", "data": "aW1hZ2U="}],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(reply["reply"], "the answer");

    let seen = gateway.provider.seen();
    let last = seen.last().expect("the model was asked");
    let message = last.messages.last().expect("the message is there");
    assert!(
        message
            .content
            .iter()
            .any(|block| matches!(block, lia::provider::types::ContentBlock::Image { .. })),
        "the picture did not reach the model"
    );
}

#[tokio::test]
async fn the_websocket_speaks_the_original_protocol() {
    let gateway = TestGateway::start().await;
    let mut socket = common::WsClient::connect(&gateway.ws_url()).await;

    // The gateway greets the client.
    let hello = next_frame(&mut socket).await;
    assert_eq!(hello["type"], "hello");

    socket
        .send_text(&json!({ "type": "send", "content": "hello" }).to_string())
        .await;

    // The turn's progress arrives as events, and then the answer.
    let mut events = 0;
    let mut done = None;
    for _ in 0..20 {
        let frame = next_frame(&mut socket).await;
        match frame["type"].as_str() {
            Some("event") => events += 1,
            Some("done") => {
                done = Some(frame);
                break;
            }
            _ => {}
        }
    }

    assert!(events > 0, "the turn produced no events");
    let done = done.expect("the turn never finished");
    assert_eq!(done["reply"], "the answer");
}

/// A request in the envelope is answered with a response carrying the same
/// id, and the turn's progress arrives as events.
#[tokio::test]
async fn the_websocket_speaks_the_request_envelope() {
    let gateway = TestGateway::start().await;
    let mut socket = common::WsClient::connect(&gateway.ws_url()).await;

    let hello = next_frame(&mut socket).await;
    assert_eq!(hello["type"], "hello");

    socket
        .send_text(
            &json!({
                "type": "req",
                "id": "42",
                "method": "message.send",
                "params": { "content": "hello" },
            })
            .to_string(),
        )
        .await;

    let mut events = 0;
    let mut response = None;
    for _ in 0..20 {
        let frame = next_frame(&mut socket).await;
        match frame["type"].as_str() {
            Some("event") => {
                events += 1;
                assert!(frame["event"].is_string(), "an event has no name: {frame}");
            }
            Some("res") => {
                response = Some(frame);
                break;
            }
            _ => {}
        }
    }

    assert!(events > 0, "the turn produced no events");
    let response = response.expect("the request was never answered");
    assert_eq!(response["id"], "42");
    assert_eq!(response["ok"], true);
    assert_eq!(response["data"]["reply"], "the answer");
    assert!(response["data"]["sessionId"].is_string());
}

/// A request for something the gateway cannot do is answered with a failure,
/// not with silence.
#[tokio::test]
async fn an_unknown_method_is_refused() {
    let gateway = TestGateway::start().await;
    let mut socket = common::WsClient::connect(&gateway.ws_url()).await;

    let _ = next_frame(&mut socket).await;

    socket
        .send_text(&json!({ "type": "req", "id": "1", "method": "nonsense" }).to_string())
        .await;

    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["type"], "res");
    assert_eq!(frame["id"], "1");
    assert_eq!(frame["ok"], false);
    assert!(frame["error"].as_str().unwrap().contains("nonsense"));
}

/// The envelope can ask what the gateway is.
#[tokio::test]
async fn a_health_request_is_answered() {
    let gateway = TestGateway::start().await;
    let mut socket = common::WsClient::connect(&gateway.ws_url()).await;

    let _ = next_frame(&mut socket).await;

    socket
        .send_text(&json!({ "type": "req", "id": "7", "method": "health" }).to_string())
        .await;

    let frame = next_frame(&mut socket).await;
    assert_eq!(frame["type"], "res");
    assert_eq!(frame["ok"], true);
    assert_eq!(frame["data"]["status"], "ok");
    assert_eq!(frame["data"]["provider"], "mock");
}

/// The sessions can be listed over the envelope.
#[tokio::test]
async fn the_sessions_can_be_listed() {
    let gateway = TestGateway::start().await;
    let mut socket = common::WsClient::connect(&gateway.ws_url()).await;

    let _ = next_frame(&mut socket).await;

    socket
        .send_text(&json!({ "type": "req", "id": "9", "method": "session.create" }).to_string())
        .await;

    let created = next_frame(&mut socket).await;
    assert_eq!(created["type"], "res");
    assert_eq!(created["ok"], true);
    assert!(created["data"]["session"]["id"].is_string());

    socket
        .send_text(&json!({ "type": "req", "id": "10", "method": "session.list" }).to_string())
        .await;

    let listed = next_frame(&mut socket).await;
    assert_eq!(listed["id"], "10");
    assert_eq!(listed["ok"], true);
    assert_eq!(
        listed["data"]["sessions"].as_array().unwrap().len(),
        1,
        "the session was not listed"
    );
}

/// Read the next frame from the socket.
async fn next_frame(socket: &mut common::WsClient) -> Value {
    let text = socket.next_text().await;
    serde_json::from_str(&text).expect("the frame is not JSON")
}

/// The interface can ask what it can offer.
#[tokio::test]
async fn the_tools_and_skills_can_be_listed() {
    let gateway = TestGateway::start().await;

    let tools: Value = reqwest::get(gateway.url("/v1/tools"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        tools["tools"].is_array(),
        "the tools are not a list: {tools}"
    );

    let skills: Value = reqwest::get(gateway.url("/v1/skills"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        skills["skills"].is_array(),
        "the skills are not a list: {skills}"
    );
}

/// The configuration can be read and edited without exposing the file.
#[tokio::test]
async fn the_configuration_can_be_read_and_edited() {
    let gateway = TestGateway::start().await;

    let before: Value = reqwest::get(gateway.url("/v1/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before["config"]["provider"], "openai");
    assert_eq!(before["config"]["model"], "");

    let client = reqwest::Client::new();
    let changed = client
        .patch(gateway.url("/v1/config"))
        .json(&json!({
            "model": "test-model",
            "temperature": "0.4",
            "tools": "off",
            "wake_word": "jarvis"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), reqwest::StatusCode::OK);

    let after: Value = reqwest::get(gateway.url("/v1/config"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["config"]["model"], "test-model");
    assert_eq!(after["config"]["temperature"], "0.4");
    assert_eq!(after["config"]["tools"], "off");
    assert_eq!(after["config"]["wake_word"], "jarvis");

    let saved = std::fs::read_to_string(&gateway.config_path).unwrap();
    assert!(saved.contains("test-model"));
    assert!(saved.contains("jarvis"));
    let _ = std::fs::remove_file(&gateway.config_path);
}

/// A bad setting is refused, and nothing is written.
#[tokio::test]
async fn a_bad_setting_is_refused() {
    let gateway = TestGateway::start().await;

    let response = reqwest::Client::new()
        .patch(gateway.url("/v1/config"))
        .json(&json!({"temperature": "hot"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(
        !gateway.config_path.exists(),
        "a refused setting wrote the file"
    );
}

#[tokio::test]
async fn inbox_data_routes_require_auth_and_keep_the_local_owner() {
    let gateway = TestGateway::start_with_token(Some("secret")).await;
    let client = reqwest::Client::new();

    let refused = client.get(gateway.url("/v1/inbox")).send().await.unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
    let refused = client
        .post(gateway.url("/v1/inbox"))
        .bearer_auth("wrong")
        .json(&json!({"text": "not stored"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);

    let response = client
        .post(gateway.url("/v1/inbox"))
        .bearer_auth("secret")
        .json(&json!({
            "text": "review the launch plan",
            "kind": "task",
            "priority": "high",
            "owner": "caller-selected-owner"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let record: Value = response.json().await.unwrap();
    assert_eq!(record["text"], "review the launch plan");
    assert_eq!(record["owner"], "local");
    let id = record["id"].as_str().unwrap();

    let listed: Value = client
        .get(gateway.url("/v1/inbox?kind=task"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], id);

    let done: Value = client
        .patch(gateway.url(&format!("/v1/inbox/{id}")))
        .bearer_auth("secret")
        .json(&json!({"state": "done"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(done["state"], "done");
    assert!(done["completed_at"].is_string());

    let deferred: Value = client
        .post(gateway.url(&format!("/v1/inbox/{id}/defer")))
        .bearer_auth("secret")
        .json(&json!({"until": "2030-04-05T06:07:08Z"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(deferred["snoozed_until"].is_string());

    let archived: Value = client
        .patch(gateway.url(&format!("/v1/inbox/{id}")))
        .bearer_auth("secret")
        .json(&json!({"state": "archived"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(archived["state"], "archived");
    assert!(archived["archived_at"].is_string());

    let visible: Value = client
        .get(gateway.url("/v1/inbox?include_archived=true"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(visible.as_array().unwrap().len(), 1);

    let deleted = client
        .delete(gateway.url(&format!("/v1/inbox/{id}")))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
    let missing = client
        .get(gateway.url(&format!("/v1/inbox/{id}")))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

/// The gateway serves the interface, in the browser.
#[tokio::test]
async fn the_page_is_served() {
    let gateway = TestGateway::start().await;

    let response = reqwest::get(gateway.url("/")).await.unwrap();

    assert_eq!(response.status(), 200, "the page was not served");
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.contains("text/html"),
        "the page is not HTML: {content_type}"
    );

    let body = response.text().await.unwrap();
    assert!(body.contains("<title>lia</title>"), "the page has no title");
    assert!(
        body.contains("/assets/core.js"),
        "the page does not load its scripts"
    );
    assert!(
        body.contains("/assets/nexus.css"),
        "the page does not load its styles"
    );

    let script = reqwest::get(gateway.url("/assets/core.js")).await.unwrap();
    assert_eq!(script.status(), 200, "the script was not served");
    let content_type = script
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.contains("javascript"),
        "the script has the wrong type: {content_type}"
    );
    let script = script.text().await.unwrap();
    assert!(
        script.contains("/v1/ws"),
        "the script does not talk to the socket"
    );

    let style = reqwest::get(gateway.url("/assets/nexus.css"))
        .await
        .unwrap();
    assert_eq!(style.status(), 200, "the stylesheet was not served");
    assert!(
        style
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("text/css"),
        "the stylesheet has the wrong type"
    );
    for asset in ["inbox.css", "inbox.js"] {
        let response = reqwest::get(gateway.url(&format!("/assets/{asset}")))
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{asset}");
    }
}

#[tokio::test]
async fn an_unknown_asset_is_not_found() {
    let gateway = TestGateway::start().await;
    let response = reqwest::get(gateway.url("/assets/not-a-file.js"))
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

/// The page is a drawing, so it is open; the rest of the gateway is not.
#[tokio::test]
async fn the_page_is_open_but_the_api_is_not() {
    let gateway = TestGateway::start_with_token(Some("secret")).await;

    let page = reqwest::get(gateway.url("/")).await.unwrap();
    assert_eq!(page.status(), 200, "the page was not served");

    let asset = reqwest::get(gateway.url("/assets/base.css")).await.unwrap();
    assert_eq!(asset.status(), 200, "the page's styles were refused");

    let refused = reqwest::get(gateway.url("/v1/sessions")).await.unwrap();
    assert_eq!(refused.status(), 401, "the api was not refused");
}

/// A browser cannot put a header on a WebSocket, so the token may be in the
/// address of the socket. It is in the address of nothing else.
#[tokio::test]
async fn a_websocket_takes_the_token_in_its_address() {
    let gateway = TestGateway::start_with_token(Some("secret")).await;

    let refused = reqwest::get(gateway.url("/v1/ws")).await.unwrap();
    assert_eq!(refused.status(), 401, "the socket was not refused");

    let mut socket =
        common::WsClient::connect(&format!("ws://{}/v1/ws?token=secret", gateway.address)).await;
    let hello = next_frame(&mut socket).await;
    assert_eq!(hello["type"], "hello");
}

/// Without a voice, the speech endpoint says so rather than returning a
/// silence, and the voice list is empty.
#[tokio::test]
async fn speech_says_when_there_is_no_voice() {
    let gateway = TestGateway::start_with(|config| config.voice.enabled = false).await;

    let response = reqwest::Client::new()
        .post(gateway.url("/v1/audio/speech"))
        .json(&json!({ "input": "hello there" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 501);

    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "the refusal says nothing: {body}"
    );

    let health: Value = reqwest::get(gateway.url("/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        health["speech"], false,
        "the gateway claims to speak with no voice: {health}"
    );

    let voices: Value = reqwest::get(gateway.url("/v1/audio/voices"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(voices["voices"], json!([]));
}

/// A request with no words, or a format that is not offered, is refused
/// before the model is asked to make anything.
#[tokio::test]
async fn a_speech_request_is_checked_before_it_is_synthesized() {
    let gateway = TestGateway::start_with(|config| {
        config.voice.speak_provider = Some("phonon".into());
        config.voice.play_command = Some(vec!["/bin/true".into()]);
    })
    .await;

    let client = reqwest::Client::new();
    let empty = client
        .post(gateway.url("/v1/audio/speech"))
        .json(&json!({ "input": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), 400, "empty words were accepted");

    let format = client
        .post(gateway.url("/v1/audio/speech"))
        .json(&json!({ "input": "hello", "response_format": "mp3" }))
        .send()
        .await
        .unwrap();
    assert_eq!(format.status(), 400, "an unknown format was accepted");

    let health: Value = reqwest::get(gateway.url("/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        health["speech"], true,
        "the gateway does not report the Phonon voice it has: {health}"
    );
}

/// A speaker that writes a wav — piper, for instance — is served to the
/// page too, so the browser hears the same voice as the terminal. Raw PCM
/// is a Phonon shape, so it is refused rather than mislabelled.
#[tokio::test]
async fn a_speaker_that_writes_a_wav_is_served_to_the_page() {
    let gateway = TestGateway::start_with(|config| {
        // `cat` echoes the words back as the "wav"; the endpoint's job is
        // to carry whatever the speaker wrote to the client unchanged.
        config.voice.speak_wav_command = Some(vec!["/bin/cat".into()]);
        config.voice.play_command = Some(vec!["/bin/true".into()]);
    })
    .await;

    let health: Value = reqwest::get(gateway.url("/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        health["speech"], true,
        "the gateway does not report the wav speaker it has: {health}"
    );

    let client = reqwest::Client::new();
    let response = client
        .post(gateway.url("/v1/audio/speech"))
        .json(&json!({ "input": "hello there" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.contains("audio/wav"),
        "the sound has the wrong type: {content_type}"
    );
    let sound = response.text().await.unwrap();
    assert!(
        sound.contains("hello there"),
        "the words were lost: {sound:?}"
    );

    let pcm = client
        .post(gateway.url("/v1/audio/speech"))
        .json(&json!({ "input": "hello", "response_format": "pcm" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pcm.status(), 400, "a wav speaker offered raw pcm");
}
