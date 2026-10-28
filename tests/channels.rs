//! Tests for the channel adapters.
//!
//! The Discord adapter is pointed at a local server that speaks the same
//! shape, so the whole path — polling, the allow-lists, not answering its
//! own messages, and sending the reply back — is checked without touching
//! the network.

use std::sync::{Arc, Mutex};

use axum::{Json, Router, routing::get};
use lia::channel::discord::DiscordChannel;
use lia::channel::{Channel, InboundMessage, OutboundMessage, split_message};
use serde_json::{Value, json};

/// What the mock Discord knows.
#[derive(Default)]
struct Fake {
    /// The messages the channel holds, newest first.
    messages: Vec<Value>,
    /// The messages the bot sent.
    sent: Vec<Value>,
}

#[tokio::test]
async fn the_discord_adapter_reads_new_messages_and_answers_them() {
    let fake = Arc::new(Mutex::new(Fake::default()));

    // An old message, which the adapter should not answer: it only starts
    // from where the conversation is when it begins.
    fake.lock().unwrap().messages.push(json!({
        "id": "100",
        "content": "an old message",
        "author": { "id": "user1", "username": "alice" },
    }));

    let (address, server) = start_server(fake.clone()).await;

    let channel = DiscordChannel::new("token", vec!["chan1".to_string()], vec![])
        .with_api_root(format!("http://{address}/api/v10"))
        .with_poll_seconds(1);

    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
    let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

    let runner = channel.clone();
    let task = tokio::spawn(async move {
        runner.run(inbound_tx, outbound_tx).await.unwrap();
    });

    // The first poll only marks where the conversation is.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // Now a new message arrives.
    fake.lock().unwrap().messages.push(json!({
        "id": "200",
        "content": "a new message",
        "author": { "id": "user1", "username": "alice" },
    }));

    let inbound = tokio::time::timeout(std::time::Duration::from_secs(10), inbound_rx.recv())
        .await
        .expect("the adapter did not deliver the message")
        .expect("the queue closed");

    assert_eq!(inbound.text, "a new message");
    assert_eq!(inbound.sender, "user1");
    assert_eq!(inbound.sender_name.as_deref(), Some("alice"));
    assert_eq!(inbound.conversation, "chan1");
    assert_eq!(inbound.channel, "discord");

    // The reply goes back to the same channel.
    channel
        .send(&OutboundMessage::new("chan1", "the answer"))
        .await
        .unwrap();

    let sent = fake.lock().unwrap().sent.clone();
    assert_eq!(sent.len(), 1, "the reply was not sent: {sent:?}");
    assert_eq!(sent[0]["content"], "the answer");

    task.abort();
    server.abort();
}

#[tokio::test]
async fn the_discord_adapter_picks_up_a_picture() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let (address, server) = start_server(fake.clone()).await;

    let channel = DiscordChannel::new("token", vec!["chan1".to_string()], vec![])
        .with_api_root(format!("http://{address}/api/v10"))
        // The fake server stands in for Discord's own picture servers.
        .with_image_hosts(vec![format!("http://{address}")])
        .with_poll_seconds(1);

    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
    let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

    let runner = channel.clone();
    let task = tokio::spawn(async move {
        runner.run(inbound_tx, outbound_tx).await.unwrap();
    });

    // The first poll only marks where the conversation is.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    fake.lock().unwrap().messages.push(json!({
        "id": "300",
        "content": "what is this?",
        "author": { "id": "user1", "username": "alice" },
        "attachments": [{
            "url": format!("http://{address}/attachment.png"),
            "filename": "shot.png",
            "content_type": "image/png",
        }],
    }));

    let inbound = tokio::time::timeout(std::time::Duration::from_secs(10), inbound_rx.recv())
        .await
        .expect("the adapter did not deliver the message")
        .expect("the queue closed");

    assert_eq!(inbound.text, "what is this?");
    assert_eq!(inbound.images.len(), 1, "the picture was not picked up");
    assert_eq!(inbound.images[0].media_type, "image/png");
    assert!(!inbound.images[0].data.is_empty());

    task.abort();
    server.abort();
}

#[tokio::test]
async fn the_discord_adapter_ignores_a_picture_from_elsewhere() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let (address, server) = start_server(fake.clone()).await;

    // The default is Discord's own servers, so a picture that points
    // somewhere else is not fetched.
    let channel = DiscordChannel::new("token", vec!["chan1".to_string()], vec![])
        .with_api_root(format!("http://{address}/api/v10"))
        .with_poll_seconds(1);

    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
    let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

    let runner = channel.clone();
    let task = tokio::spawn(async move {
        runner.run(inbound_tx, outbound_tx).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    fake.lock().unwrap().messages.push(json!({
        "id": "301",
        "content": "look at this",
        "author": { "id": "user1", "username": "alice" },
        "attachments": [{
            "url": format!("http://{address}/attachment.png"),
            "filename": "shot.png",
            "content_type": "image/png",
        }],
    }));

    let inbound = tokio::time::timeout(std::time::Duration::from_secs(10), inbound_rx.recv())
        .await
        .expect("the adapter did not deliver the message")
        .expect("the queue closed");

    assert_eq!(inbound.text, "look at this");
    assert!(
        inbound.images.is_empty(),
        "a picture from another host was fetched"
    );

    task.abort();
    server.abort();
}

#[tokio::test]
async fn the_discord_adapter_does_not_answer_itself() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let (address, server) = start_server(fake.clone()).await;

    // The bot's own id is on the user list, so the only reason its messages
    // are ignored is the rule against answering itself.
    let channel = DiscordChannel::new(
        "token",
        vec!["chan1".to_string()],
        vec!["alice".to_string(), "bot1".to_string()],
    )
    .with_api_root(format!("http://{address}/api/v10"))
    .with_poll_seconds(1);

    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
    let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

    let runner = channel.clone();
    let task = tokio::spawn(async move {
        runner.run(inbound_tx, outbound_tx).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // The bot's own message, from the user id the fake reports as the bot.
    fake.lock().unwrap().messages.push(json!({
        "id": "300",
        "content": "the bot talking to itself",
        "author": { "id": "bot1", "username": "lia" },
    }));

    // A message from someone who is not on the user list.
    fake.lock().unwrap().messages.push(json!({
        "id": "301",
        "content": "from a stranger",
        "author": { "id": "stranger", "username": "eve" },
    }));

    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;

    assert!(
        inbound_rx.try_recv().is_err(),
        "the adapter answered a message it should not have"
    );

    task.abort();
    server.abort();
}

#[tokio::test]
async fn the_discord_adapter_only_listens_to_allowed_users() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let (address, server) = start_server(fake.clone()).await;

    let channel = DiscordChannel::new(
        "token",
        vec!["chan1".to_string()],
        vec!["alice".to_string()],
    )
    .with_api_root(format!("http://{address}/api/v10"))
    .with_poll_seconds(1);

    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
    let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

    let runner = channel.clone();
    let task = tokio::spawn(async move {
        runner.run(inbound_tx, outbound_tx).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    fake.lock().unwrap().messages.push(json!({
        "id": "400",
        "content": "from a stranger",
        "author": { "id": "eve", "username": "eve" },
    }));
    fake.lock().unwrap().messages.push(json!({
        "id": "401",
        "content": "from alice",
        "author": { "id": "alice", "username": "alice" },
    }));

    let inbound = tokio::time::timeout(std::time::Duration::from_secs(10), inbound_rx.recv())
        .await
        .expect("the allowed user's message did not arrive")
        .expect("the queue closed");

    assert_eq!(inbound.text, "from alice");

    task.abort();
    server.abort();
}

#[test]
fn a_long_message_is_split_at_line_boundaries() {
    let text = (0..100)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");

    let chunks = split_message(&text, 100);
    assert!(chunks.len() > 1, "the message was not split");
    for chunk in &chunks {
        assert!(chunk.len() <= 100, "a chunk was too long: {}", chunk.len());
    }
    // Nothing is lost.
    assert_eq!(chunks.concat(), text);
}

#[test]
fn a_short_message_is_left_alone() {
    let chunks = split_message("hello", 100);
    assert_eq!(chunks, vec!["hello".to_string()]);
}

/// Start a server that speaks the Discord API's shape.
async fn start_server(
    fake: Arc<Mutex<Fake>>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    async fn me() -> Json<Value> {
        Json(json!({ "id": "bot1", "username": "lia" }))
    }

    async fn messages(
        axum::extract::Path(_channel): axum::extract::Path<String>,
        axum::extract::State(fake): axum::extract::State<Arc<Mutex<Fake>>>,
    ) -> Json<Vec<Value>> {
        let messages = fake.lock().unwrap().messages.clone();
        Json(messages)
    }

    async fn send(
        axum::extract::Path(_channel): axum::extract::Path<String>,
        axum::extract::State(fake): axum::extract::State<Arc<Mutex<Fake>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        fake.lock().unwrap().sent.push(body);
        Json(json!({ "id": "1" }))
    }

    async fn attachment() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\n a picture".to_vec()
    }

    let app = Router::new()
        .route("/api/v10/users/@me", get(me))
        .route(
            "/api/v10/channels/{channel}/messages",
            get(messages).post(send),
        )
        .route("/attachment.png", get(attachment))
        .with_state(fake);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (address, handle)
}

/// The WhatsApp tests, against a server that speaks the Cloud API's shape
/// and a webhook that is exercised the way Meta exercises it.
mod whatsapp {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, routing::post};
    use hmac::{Hmac, Mac};
    use lia::channel::whatsapp::WhatsAppChannel;
    use lia::channel::{Channel, InboundMessage, OutboundMessage};
    use serde_json::{Value, json};
    use sha2::Sha256;

    /// What the mock Graph API knows.
    #[derive(Default)]
    struct Fake {
        sent: Vec<Value>,
    }

    #[tokio::test]
    async fn a_reply_is_sent_through_the_cloud_api() {
        let fake = Arc::new(Mutex::new(Fake::default()));
        let (address, server) = start_server(fake.clone()).await;

        let channel = WhatsAppChannel::new(
            "token",
            "123456789",
            "verify",
            vec!["15551234567".to_string()],
        )
        .with_api_root(format!("http://{address}/v21.0"));

        channel
            .send(&OutboundMessage::new("+1 555 123 4567", "the answer"))
            .await
            .expect("the reply is sent");

        let sent = fake.lock().unwrap().sent.clone();
        assert_eq!(sent.len(), 1, "the reply was not sent: {sent:?}");
        assert_eq!(sent[0]["messaging_product"], "whatsapp");
        assert_eq!(sent[0]["recipient_type"], "individual");
        assert_eq!(sent[0]["to"], "15551234567");
        assert_eq!(sent[0]["type"], "text");
        assert_eq!(sent[0]["text"]["body"], "the answer");

        server.abort();
    }

    #[tokio::test]
    async fn the_webhook_verifies_and_delivers_allowed_messages() {
        let channel = WhatsAppChannel::new(
            "token",
            "123456789",
            "verify-me",
            vec!["+1 (555) 123-4567".to_string()],
        )
        .with_app_secret("app-secret")
        .with_webhook("127.0.0.1", 0);

        let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

        let runner = channel.clone();
        let task = tokio::spawn(async move {
            runner.run(inbound_tx, outbound_tx).await.unwrap();
        });

        let address = wait_for_webhook(&channel).await;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/webhooks/whatsapp");

        // The subscription check is answered with the challenge.
        let response = client
            .get(&url)
            .query(&[
                ("hub.mode", "subscribe"),
                ("hub.verify_token", "verify-me"),
                ("hub.challenge", "the-challenge"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "the-challenge");

        // The wrong token is refused.
        let refused = client
            .get(&url)
            .query(&[
                ("hub.mode", "subscribe"),
                ("hub.verify_token", "not-the-token"),
                ("hub.challenge", "the-challenge"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 403);

        // A delivery from an allowed number is signed and delivered.
        let allowed = delivery("+1 555 123 4567", "Alice", "hello there");
        let response = deliver(&client, &url, &allowed, Some("app-secret")).await;
        assert_eq!(response.status(), 200);

        let inbound = tokio::time::timeout(std::time::Duration::from_secs(5), inbound_rx.recv())
            .await
            .expect("the adapter did not deliver the message")
            .expect("the queue closed");
        assert_eq!(inbound.text, "hello there");
        assert_eq!(inbound.sender, "15551234567");
        assert_eq!(inbound.sender_name.as_deref(), Some("Alice"));
        assert_eq!(inbound.conversation, "15551234567");
        assert_eq!(inbound.channel, "whatsapp");

        // A delivery from somebody else is acknowledged but not passed on.
        let stranger = delivery("15550000000", "Eve", "let me in");
        let response = deliver(&client, &url, &stranger, Some("app-secret")).await;
        assert_eq!(response.status(), 200);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(
            inbound_rx.try_recv().is_err(),
            "a stranger's message reached the assistant"
        );

        // A body that is not signed by the app secret is refused.
        let forged = delivery("15551234567", "Alice", "forged");
        let response = deliver(&client, &url, &forged, Some("another-secret")).await;
        assert_eq!(response.status(), 403);

        channel.shutdown().await.unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
    }

    /// Wait until the adapter has bound its webhook.
    async fn wait_for_webhook(channel: &WhatsAppChannel) -> std::net::SocketAddr {
        for _ in 0..200 {
            if let Some(address) = channel.webhook_address() {
                return address;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the webhook did not start");
    }

    /// A webhook delivery from one number.
    fn delivery(from: &str, name: &str, text: &str) -> String {
        json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "1",
                "changes": [{
                    "field": "messages",
                    "value": {
                        "messaging_product": "whatsapp",
                        "contacts": [{
                            "wa_id": from,
                            "profile": { "name": name }
                        }],
                        "messages": [{
                            "from": from,
                            "id": "wamid.1",
                            "timestamp": "1700000000",
                            "type": "text",
                            "text": { "body": text }
                        }]
                    }
                }]
            }]
        })
        .to_string()
    }

    /// Sign a body and post it, the way Meta does.
    async fn deliver(
        client: &reqwest::Client,
        url: &str,
        body: &str,
        secret: Option<&str>,
    ) -> reqwest::Response {
        let mut request = client
            .post(url)
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(secret) = secret {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(body.as_bytes());
            let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
            request = request.header("x-hub-signature-256", signature);
        }
        request.send().await.unwrap()
    }

    /// Start a server that speaks the Graph API's shape.
    async fn start_server(
        fake: Arc<Mutex<Fake>>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        async fn send(
            axum::extract::Path(_phone): axum::extract::Path<String>,
            axum::extract::State(fake): axum::extract::State<Arc<Mutex<Fake>>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            fake.lock().unwrap().sent.push(body);
            Json(json!({ "messages": [{ "id": "wamid.1" }] }))
        }

        let app = Router::new()
            .route("/v21.0/{phone}/messages", post(send))
            .with_state(fake);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        (address, handle)
    }
}

/// The Slack tests, against a server that speaks the Web API's shape.
mod slack {
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, routing::get};
    use lia::channel::slack::SlackChannel;
    use lia::channel::{Channel, InboundMessage, OutboundMessage};
    use serde_json::{Value, json};

    /// What the mock Slack knows.
    #[derive(Default)]
    struct Fake {
        /// The messages the conversation holds, newest first.
        messages: Vec<Value>,
        /// The messages the bot sent.
        sent: Vec<Value>,
    }

    #[tokio::test]
    async fn the_slack_adapter_reads_new_messages_and_answers_them() {
        let fake = Arc::new(Mutex::new(Fake::default()));
        fake.lock().unwrap().messages.push(json!({
            "type": "message",
            "user": "U1",
            "text": "an old message",
            "ts": "100.000100",
        }));

        let (address, server) = start_server(fake.clone()).await;

        let channel = SlackChannel::new("token", vec!["C1".to_string()], vec![])
            .with_api_root(format!("http://{address}/api"))
            .with_poll_seconds(1);

        let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

        let runner = channel.clone();
        let task = tokio::spawn(async move {
            runner.run(inbound_tx, outbound_tx).await.unwrap();
        });

        // The first poll only marks where the conversation is.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        fake.lock().unwrap().messages.push(json!({
            "type": "message",
            "user": "U1",
            "text": "a new message",
            "ts": "200.000200",
        }));

        let inbound = tokio::time::timeout(std::time::Duration::from_secs(10), inbound_rx.recv())
            .await
            .expect("the adapter did not deliver the message")
            .expect("the queue closed");

        assert_eq!(inbound.text, "a new message");
        assert_eq!(inbound.sender, "U1");
        assert_eq!(inbound.conversation, "C1");
        assert_eq!(inbound.channel, "slack");

        // The reply goes back to the same conversation.
        channel
            .send(&OutboundMessage::new("C1", "the answer"))
            .await
            .unwrap();

        let sent = fake.lock().unwrap().sent.clone();
        assert_eq!(sent.len(), 1, "the reply was not sent: {sent:?}");
        assert_eq!(sent[0]["text"], "the answer");
        assert_eq!(sent[0]["channel"], "C1");

        task.abort();
        server.abort();
    }

    #[tokio::test]
    async fn the_slack_adapter_ignores_bots_and_strangers() {
        let fake = Arc::new(Mutex::new(Fake::default()));
        let (address, server) = start_server(fake.clone()).await;

        let channel = SlackChannel::new("token", vec!["C1".to_string()], vec!["U1".to_string()])
            .with_api_root(format!("http://{address}/api"))
            .with_poll_seconds(1);

        let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<InboundMessage>(16);
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel::<OutboundMessage>(16);

        let runner = channel.clone();
        let task = tokio::spawn(async move {
            runner.run(inbound_tx, outbound_tx).await.unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        // A message from another bot.
        fake.lock().unwrap().messages.push(json!({
            "type": "message",
            "bot_id": "B1",
            "text": "from a bot",
            "ts": "300.000300",
        }));
        // A message from someone who is not on the user list.
        fake.lock().unwrap().messages.push(json!({
            "type": "message",
            "user": "U9",
            "text": "from a stranger",
            "ts": "301.000301",
        }));
        // And one from the bot itself.
        fake.lock().unwrap().messages.push(json!({
            "type": "message",
            "user": "U0",
            "text": "the bot talking to itself",
            "ts": "302.000302",
        }));

        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;

        assert!(
            inbound_rx.try_recv().is_err(),
            "the adapter answered a message it should not have"
        );

        task.abort();
        server.abort();
    }

    /// Start a server that speaks the Slack Web API's shape.
    async fn start_server(
        fake: Arc<Mutex<Fake>>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        async fn me() -> Json<Value> {
            Json(json!({ "ok": true, "user": { "id": "U0" } }))
        }

        async fn history(
            axum::extract::Query(query): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >,
            axum::extract::State(fake): axum::extract::State<Arc<Mutex<Fake>>>,
        ) -> Json<Value> {
            let messages = fake.lock().unwrap().messages.clone();
            Json(json!({ "ok": true, "messages": messages, "channel": query }))
        }

        async fn post(
            axum::extract::State(fake): axum::extract::State<Arc<Mutex<Fake>>>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            fake.lock().unwrap().sent.push(body);
            Json(json!({ "ok": true }))
        }

        let app = Router::new()
            .route("/api/users.info", get(me))
            .route("/api/conversations.history", get(history))
            .route("/api/chat.postMessage", axum::routing::post(post))
            .with_state(fake);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        (address, handle)
    }
}
