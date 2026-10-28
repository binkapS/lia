//! Tests of the permission requests a chat can answer.
//!
//! The assistant asks in the conversation when a tool needs permission, and
//! the person who asked it something answers with `/approve` or `/deny`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lia::agent::session::SessionStore;
use lia::agent::{Agent, AgentConfig, SessionRunner};
use lia::channel::{ChannelDispatcher, InboundMessage};
use lia::tool::ToolRegistry;

/// A runner, a channel and a dispatcher over them.
struct Chat {
    dispatcher: ChannelDispatcher,
    channel: Arc<common::FakeChannel>,
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Chat {
    /// A chat whose assistant wants to run a tool that needs permission.
    fn new() -> Chat {
        let provider = Arc::new(common::MockProvider::with_tool(
            "do_thing",
            serde_json::json!({ "x": 1 }),
        ));

        let calls = Arc::new(Mutex::new(Vec::new()));
        let tool = Arc::new(common::RecordingTool {
            name: "do_thing",
            calls: calls.clone(),
            requires_approval: true,
        });

        let mut registry = ToolRegistry::new();
        registry.register(tool);

        let agent = Agent::new(provider, Arc::new(registry), AgentConfig::default());
        let runner = SessionRunner::new(
            Arc::new(agent),
            Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
        );

        let channel = Arc::new(common::FakeChannel::new());
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);

        let dispatcher = ChannelDispatcher::new(runner, event_tx, channel.clone())
            .with_approval_timeout(Duration::from_millis(500));

        Chat {
            dispatcher,
            channel,
            calls,
        }
    }

    /// The messages the assistant sent to the chat.
    fn sent(&self) -> Vec<String> {
        self.channel
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }
}

/// Wait for the permission request to reach the chat.
async fn wait_for_the_question(chat: &Chat) -> String {
    for _ in 0..100 {
        if let Some(text) = chat.sent().into_iter().find(|t| t.contains("/approve")) {
            return text;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the assistant never asked for permission");
}

#[tokio::test]
async fn a_tool_that_needs_permission_is_asked_for_in_the_chat() {
    let chat = Chat::new();

    // The assistant starts working.
    let dispatcher = chat.dispatcher.clone();
    let turn = tokio::spawn(async move {
        dispatcher
            .handle(InboundMessage::new(
                "fake",
                "alice",
                "do the thing",
                "chat1",
            ))
            .await
    });

    // It asks in the chat, and the question says how to answer it.
    let question = wait_for_the_question(&chat).await;
    assert!(
        question.contains("do_thing"),
        "the question is unclear: {question}"
    );
    assert!(
        question.contains("/approve 1"),
        "the question has no id: {question}"
    );

    // Alice answers.
    let reply = chat
        .dispatcher
        .handle_command(&InboundMessage::new("fake", "alice", "/approve 1", "chat1"))
        .await;
    assert_eq!(reply.as_deref(), Some("Done."));

    // The tool ran, and the turn finished.
    let answer = turn.await.unwrap();
    assert!(answer.is_some(), "the turn produced no reply");
    assert_eq!(chat.calls.lock().unwrap().len(), 1, "the tool did not run");
}

#[tokio::test]
async fn a_refusal_stops_the_tool() {
    let chat = Chat::new();

    let dispatcher = chat.dispatcher.clone();
    let turn = tokio::spawn(async move {
        dispatcher
            .handle(InboundMessage::new(
                "fake",
                "alice",
                "do the thing",
                "chat1",
            ))
            .await
    });

    let _ = wait_for_the_question(&chat).await;

    let reply = chat
        .dispatcher
        .handle_command(&InboundMessage::new("fake", "alice", "/deny 1", "chat1"))
        .await;
    assert_eq!(reply.as_deref(), Some("Done."));

    let _ = turn.await.unwrap();
    assert!(chat.calls.lock().unwrap().is_empty(), "the tool ran anyway");
}

#[tokio::test]
async fn only_the_person_who_asked_may_answer() {
    let chat = Chat::new();

    let dispatcher = chat.dispatcher.clone();
    let turn = tokio::spawn(async move {
        dispatcher
            .handle(InboundMessage::new(
                "fake",
                "alice",
                "do the thing",
                "chat1",
            ))
            .await
    });

    let _ = wait_for_the_question(&chat).await;

    // Somebody else in the channel tries to answer.
    let reply = chat
        .dispatcher
        .handle_command(&InboundMessage::new("fake", "eve", "/approve 1", "chat1"))
        .await;
    assert_eq!(
        reply.as_deref(),
        Some("There is no tool waiting for that answer.")
    );

    // Nobody answered in time, so the tool did not run.
    let _ = turn.await.unwrap();
    assert!(
        chat.calls.lock().unwrap().is_empty(),
        "the tool ran without an answer"
    );
}

#[tokio::test]
async fn an_answer_may_name_the_request() {
    let chat = Chat::new();

    let dispatcher = chat.dispatcher.clone();
    let turn = tokio::spawn(async move {
        dispatcher
            .handle(InboundMessage::new(
                "fake",
                "alice",
                "do the thing",
                "chat1",
            ))
            .await
    });

    let _ = wait_for_the_question(&chat).await;

    // An answer for a request that is not waiting changes nothing.
    let reply = chat
        .dispatcher
        .handle_command(&InboundMessage::new(
            "fake",
            "alice",
            "/approve 99",
            "chat1",
        ))
        .await;
    assert_eq!(
        reply.as_deref(),
        Some("There is no tool waiting for that answer.")
    );

    // The right one does.
    let reply = chat
        .dispatcher
        .handle_command(&InboundMessage::new("fake", "alice", "/approve 1", "chat1"))
        .await;
    assert_eq!(reply.as_deref(), Some("Done."));

    let _ = turn.await.unwrap();
    assert_eq!(chat.calls.lock().unwrap().len(), 1, "the tool did not run");
}

#[tokio::test]
async fn the_answer_may_cover_the_rest_of_the_session() {
    let chat = Chat::new();

    let dispatcher = chat.dispatcher.clone();
    let turn = tokio::spawn(async move {
        dispatcher
            .handle(InboundMessage::new(
                "fake",
                "alice",
                "do the thing",
                "chat1",
            ))
            .await
    });

    let _ = wait_for_the_question(&chat).await;

    let reply = chat
        .dispatcher
        .handle_command(&InboundMessage::new(
            "fake",
            "alice",
            "/approve 1 always",
            "chat1",
        ))
        .await;
    assert_eq!(reply.as_deref(), Some("Done."));

    let _ = turn.await.unwrap();
    assert_eq!(chat.calls.lock().unwrap().len(), 1, "the tool did not run");
}
