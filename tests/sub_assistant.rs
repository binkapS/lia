//! Tests of the work that is handed to another assistant.
//!
//! The other assistant works in a conversation of its own, and the person
//! who asked the question should be able to see that it is making progress.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lia::agent::session::SessionStore;
use lia::agent::{Agent, AgentConfig, AgentEvent, SessionRunner};
use lia::provider::types::{
    CompletionRequest, CompletionResponse, ContentBlock, ModelInfo, StopReason, StreamEvent,
};
use lia::provider::{EventStream, Provider, Result};
use lia::tool::ToolRegistry;
use lia::tool::task::TaskTool;
use serde_json::json;

/// A provider that replies with a scripted sequence.
struct ScriptedProvider {
    /// Each entry is the tool to ask for, or `None` for a plain reply.
    script: Mutex<Vec<Option<(String, serde_json::Value)>>>,
    /// The requests the provider received.
    seen: Mutex<Vec<CompletionRequest>>,
}

impl ScriptedProvider {
    fn new(script: Vec<Option<(String, serde_json::Value)>>) -> Self {
        ScriptedProvider {
            script: Mutex::new(script),
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    fn model(&self) -> &str {
        "scripted-model"
    }

    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        let mut script = self.script.lock().unwrap();
        let step = script.first().cloned();
        if step.is_some() {
            script.remove(0);
        }

        let content = match step.flatten() {
            Some((name, input)) => vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name,
                input,
            }],
            None => vec![ContentBlock::text("done")],
        };

        Ok(CompletionResponse {
            content,
            model: "scripted-model".into(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        self.seen.lock().unwrap().push(request.clone());
        let response = self.complete(request).await?;
        let stream = async_stream::stream! {
            for block in response.content {
                match block {
                    ContentBlock::Text { text } => {
                        yield Ok(StreamEvent::text(text));
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        yield Ok(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
                        yield Ok(StreamEvent::ToolCallDelta {
                            id: id.clone(),
                            arguments: input.to_string(),
                        });
                        yield Ok(StreamEvent::ToolCallEnd { id });
                    }
                    _ => {}
                }
            }
            yield Ok(StreamEvent::Done {
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            });
        };
        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo::new("scripted-model", 100_000)])
    }
}

/// The events of a turn, and the answer.
async fn run(runner: Arc<SessionRunner>) -> Vec<AgentEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
    let handle = tokio::spawn(async move {
        runner
            .run("main", Some("please do the work".to_string()), tx)
            .await;
    });

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let _ = handle.await;
    events
}

#[tokio::test]
async fn the_work_of_another_assistant_is_shown() {
    // The main assistant hands the work over; the other one answers; the
    // main one has the last word.
    let provider = Arc::new(ScriptedProvider::new(vec![
        Some((
            "task".to_string(),
            json!({ "prompt": "read the file and say what is in it", "label": "reading a file" }),
        )),
        None,
        None,
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TaskTool::new()));
    let agent = Agent::new(provider, Arc::new(registry), AgentConfig::default());
    let runner = SessionRunner::new(
        Arc::new(agent),
        Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
    );

    let events = run(runner).await;

    let notes: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::SubAssistant { label, note } => {
                assert_eq!(label, "reading a file");
                Some(note.as_str())
            }
            _ => None,
        })
        .collect();

    assert!(
        !notes.is_empty(),
        "nothing was said about the other assistant: {events:?}"
    );
    assert!(
        notes.iter().any(|note| note.contains("done")),
        "the other assistant's answer was not shown: {notes:?}"
    );
}

#[tokio::test]
async fn the_other_assistant_works_in_a_conversation_of_its_own() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("task".to_string(), json!({ "prompt": "do the work" }))),
        None,
        None,
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TaskTool::new()));
    let agent = Agent::new(provider.clone(), Arc::new(registry), AgentConfig::default());
    let runner = SessionRunner::new(
        Arc::new(agent),
        Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
    );

    let _events = run(runner).await;

    // The other assistant was asked in a session of its own, and the task
    // tool was not offered to it.
    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "the model was not asked three times");
    let sub = seen.get(1).expect("the other assistant was asked");
    assert!(
        sub.messages.len() == 1,
        "the other assistant saw more than its brief"
    );
    let offered = sub.tools.len();
    assert!(
        !sub.tools.iter().any(|tool| tool.name == "task"),
        "the other assistant was offered the task tool: {offered}"
    );
}
