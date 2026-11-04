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

/// The WhatsApp tests. The adapter links by qr code, and there is nothing
/// to listen on: what is answerable offline is that an unlinked reply asks
/// for the scan instead of lying about delivery, and that a pairing code
/// lets a stranger's number in the way Telegram pairs a chat.
mod whatsapp {
    use std::sync::Arc;

    use lia::channel::whatsapp::WhatsAppChannel;
    use lia::channel::{Channel, OutboundMessage, PairingStore};

    #[tokio::test]
    async fn a_reply_without_a_link_asks_for_the_qr_code() {
        let state = tempfile::tempdir().unwrap();
        let channel = WhatsAppChannel::new(
            state.path().join("whatsapp.db"),
            vec!["15551234567".to_string()],
        );

        let error = channel
            .send(&OutboundMessage::new("15551234567", "the answer"))
            .await
            .expect_err("an adapter that was never linked sends nothing");

        assert!(
            error.to_string().contains("qr"),
            "the error should point at the qr code: {error}"
        );
    }

    #[tokio::test]
    async fn a_pairing_code_lets_a_number_in() {
        let state = tempfile::tempdir().unwrap();
        let store =
            Arc::new(PairingStore::open(state.path().join("pairing.db")).expect("the store opens"));

        let channel = WhatsAppChannel::new(state.path().join("whatsapp.db"), Vec::new())
            .with_pairing(store.clone());

        assert!(
            !channel.is_authorized("15551234567"),
            "nobody is let in before a pairing is approved"
        );

        let (request, _) = store.request("whatsapp", "15551234567", "15551234567", None);
        store
            .approve(&request.code)
            .expect("a waiting request is approved");

        assert!(
            channel.is_authorized("15551234567"),
            "an approved pairing lets the number through"
        );
        assert!(
            !channel.is_authorized("15550000000"),
            "a stranger is still kept out"
        );
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
