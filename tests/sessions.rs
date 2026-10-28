//! Tests of the session store and the channel dispatcher.
//!
//! A mock provider stands in for a model, so what the assistant remembers —
//! and what it forgets when it is told to — can be checked without a network.

mod common;

use std::sync::Arc;

use lia::agent::session::SessionStore;
use lia::agent::{Agent, AgentConfig, SessionRunner};
use lia::channel::{ChannelDispatcher, InboundMessage};
use lia::provider::types::ContentBlock;
use lia::tool::ToolRegistry;

/// A runner over a mock provider.
fn runner(provider: Arc<common::MockProvider>, store: SessionStore) -> Arc<SessionRunner> {
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
    );
    SessionRunner::new(Arc::new(agent), Arc::new(tokio::sync::Mutex::new(store)))
}

/// Run a turn and wait for it to finish.
async fn say(runner: Arc<SessionRunner>, session: &str, text: &str) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let runner = runner.clone();
    let session = session.to_string();
    let text = text.to_string();
    let handle = tokio::spawn(async move {
        runner.run(&session, Some(text), tx).await;
    });
    while rx.recv().await.is_some() {}
    handle.await.unwrap();
}

#[tokio::test]
async fn a_session_keeps_what_was_said() {
    let provider = Arc::new(common::MockProvider::new());
    let runner = runner(provider, SessionStore::in_memory());

    say(runner.clone(), "chat:1", "hello").await;
    say(runner.clone(), "chat:1", "and again").await;

    let messages = runner.session_messages("chat:1").await.unwrap();
    assert_eq!(messages.len(), 4, "the turn was not kept: {messages:?}");
}

#[tokio::test]
async fn a_session_is_saved_and_can_be_read_back() {
    let dir = tempfile::tempdir().unwrap();

    let provider = Arc::new(common::MockProvider::new());
    let runner = runner(provider, SessionStore::open(dir.path()).unwrap());
    say(runner.clone(), "chat:1", "remember this").await;

    // A fresh store reads what the other one wrote.
    let store = SessionStore::open(dir.path()).unwrap();
    let session = store.get("chat:1").expect("the session was not saved");
    assert!(
        session.messages.iter().any(|m| m.content.iter().any(|b| {
            matches!(b, ContentBlock::Text { text } if text.contains("remember this"))
        })),
        "the message was not saved: {:?}",
        session.messages
    );
}

#[tokio::test]
async fn resetting_a_session_forgets_it() {
    let provider = Arc::new(common::MockProvider::new());
    let runner = runner(provider, SessionStore::in_memory());

    say(runner.clone(), "chat:1", "hello").await;
    assert!(!runner.session_messages("chat:1").await.unwrap().is_empty());

    assert!(runner.reset_session("chat:1").await, "nothing was reset");

    let messages = runner.session_messages("chat:1").await.unwrap();
    assert!(
        messages.is_empty(),
        "the session was not cleared: {messages:?}"
    );

    // There is nothing left to forget.
    assert!(!runner.reset_session("chat:1").await);
}

#[tokio::test]
async fn a_reset_command_answers_the_user_and_clears_the_conversation() {
    let provider = Arc::new(common::MockProvider::new());
    let runner = runner(provider.clone(), SessionStore::in_memory());

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let channel = Arc::new(common::FakeChannel::new());
    let dispatcher = ChannelDispatcher::new(runner.clone(), event_tx, channel);

    // A first message, so the conversation has something to forget.
    let reply = dispatcher
        .handle(InboundMessage::new("discord", "alice", "hello", "chan1"))
        .await;
    assert_eq!(reply.as_deref(), Some("the answer"));
    assert_eq!(provider.requests(), 1);

    // The reset is answered by the assistant, not the model.
    let reply = dispatcher
        .handle(InboundMessage::new("discord", "alice", "/reset", "chan1"))
        .await;
    assert!(
        reply
            .as_ref()
            .is_some_and(|r| r.contains("new conversation")),
        "the reset was not answered: {reply:?}"
    );
    assert_eq!(provider.requests(), 1, "the model was asked after a reset");

    // And the conversation is empty.
    let messages = runner.session_messages("discord:chan1").await.unwrap();
    assert!(
        messages.is_empty(),
        "the session was not cleared: {messages:?}"
    );
}
