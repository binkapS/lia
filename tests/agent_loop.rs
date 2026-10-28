//! End-to-end tests of the agent loop.
//!
//! A mock provider stands in for a real model so the loop's behaviour can be
//! checked deterministically: does it call the right tool, feed the result
//! back, and stop when the model stops asking for tools?

use std::sync::Arc;

use async_trait::async_trait;
use lia::agent::{Agent, AgentConfig, AgentEvent, ApprovalHandler};
use lia::provider::types::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ModelInfo, StopReason,
    StreamEvent, ToolSpec,
};
use lia::provider::{EventStream, Provider, ProviderError, Result};
use lia::tool::approval::ApprovalDecision;
use lia::tool::filesystem::{ReadFileTool, Workspace};
use lia::tool::{Tool, ToolOutput, ToolRegistry};
use serde_json::json;
use std::sync::Mutex;

/// A provider that replies with a scripted sequence of responses.
struct ScriptedProvider {
    /// Each entry is the tool call the provider asks for, or `None` for a
    /// plain text reply.
    script: Mutex<Vec<Option<(String, serde_json::Value)>>>,
    /// Records the requests the agent made, so tests can assert on them.
    seen: Mutex<Vec<CompletionRequest>>,
    /// Text returned when the script is exhausted.
    final_text: String,
}

impl ScriptedProvider {
    fn new(script: Vec<Option<(String, serde_json::Value)>>) -> Self {
        ScriptedProvider {
            script: Mutex::new(script),
            seen: Mutex::new(Vec::new()),
            final_text: "done".into(),
        }
    }

    fn requests(&self) -> Vec<CompletionRequest> {
        self.seen.lock().unwrap().clone()
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
        let step = if script.is_empty() {
            None
        } else {
            script.remove(0)
        };

        let content = match step {
            Some((name, input)) => vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name,
                input,
            }],
            None => vec![ContentBlock::Text {
                text: self.final_text.clone(),
            }],
        };

        Ok(CompletionResponse {
            content,
            model: "scripted-model".into(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        // Record the request, then delegate to `complete` for the content.
        let response = self.complete(request).await?;
        self.seen.lock().unwrap().push(request.clone());

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

/// A tool that records that it ran.
struct RecordingTool {
    name: &'static str,
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
    requires_approval: bool,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "a test tool"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    fn requires_approval(&self) -> bool {
        self.requires_approval
    }

    async fn run(&self, args: serde_json::Value) -> lia::tool::Result<ToolOutput> {
        self.calls.lock().unwrap().push(args.clone());
        Ok(ToolOutput::ok(format!("ran {} with {args}", self.name)))
    }
}

/// A tool that always fails, used to check the loop detection.
struct FailingTool {
    name: &'static str,
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "a test tool that never works"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn run(&self, args: serde_json::Value) -> lia::tool::Result<ToolOutput> {
        self.calls.lock().unwrap().push(args.clone());
        Ok(ToolOutput::error("it did not work"))
    }
}

/// An approver that records its decisions.
struct RecordingApprover {
    decisions: Arc<Mutex<Vec<(String, ApprovalDecision)>>>,
}

#[async_trait]
impl ApprovalHandler for RecordingApprover {
    async fn request(
        &self,
        request: lia::tool::approval::ApprovalRequest,
    ) -> Option<ApprovalDecision> {
        let decision = ApprovalDecision::Approve;
        self.decisions
            .lock()
            .unwrap()
            .push((request.tool.clone(), decision));
        Some(decision)
    }
}

/// An approver that always approves, used to check the approval path.
fn approving_handler() -> Arc<dyn ApprovalHandler> {
    Arc::new(RecordingApprover {
        decisions: Arc::new(Mutex::new(Vec::new())),
    })
}

fn registry_with(tool: Arc<dyn Tool>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(tool);
    registry
}

#[tokio::test]
async fn the_loop_calls_a_tool_and_feeds_the_result_back() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "do_thing",
        calls: Arc::clone(&calls),
        requires_approval: false,
    });

    // First the model asks for the tool, then it answers with text.
    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("do_thing".into(), json!({ "x": 1 }))),
        None,
    ]));

    let agent = Agent::new(
        provider.clone(),
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let messages = vec![Message::user("please do the thing")];

    let handle =
        tokio::spawn(async move { agent.run_turn(messages, "test-session", None, tx).await });

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        let done = matches!(event, AgentEvent::Finished { .. });
        events.push(event);
        if done {
            break;
        }
    }

    let (final_messages, _, iterations) = handle.await.unwrap();

    // The tool ran exactly once with the arguments the model supplied.
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["x"], 1);
    drop(recorded);

    // The tool result was fed back to the provider.
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "the model should be called twice");
    let second = &requests[1];
    assert!(
        second.messages.iter().any(|m| m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))),
        "the second request should carry the tool result"
    );

    // The final transcript has the user's message, the tool call, the tool
    // result, and the assistant's reply.
    assert_eq!(final_messages.len(), 4);
    assert_eq!(final_messages[0].role, lia::provider::types::Role::User);
    assert_eq!(final_messages[3].text(), "done");
    assert_eq!(iterations, 2);

    // The UI saw the tool call and its result.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallStarted { name, .. } if name == "do_thing"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallFinished { name, .. } if name == "do_thing"
    )));
}

#[tokio::test]
async fn a_denied_tool_is_not_run() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "dangerous",
        calls: Arc::clone(&calls),
        requires_approval: true,
    });

    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("dangerous".into(), json!({}))),
        None,
    ]));

    let agent = Agent::new(
        provider,
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    struct Deny;
    #[async_trait]
    impl ApprovalHandler for Deny {
        async fn request(
            &self,
            _request: lia::tool::approval::ApprovalRequest,
        ) -> Option<ApprovalDecision> {
            Some(ApprovalDecision::Deny)
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "s", Some(&Deny), tx)
            .await
    });

    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    let (messages, _, _) = handle.await.unwrap();

    assert!(calls.lock().unwrap().is_empty(), "the tool must not run");

    // The model was told the call was denied, so it can recover.
    let tool_result = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .expect("a tool result should be present");
    assert!(tool_result.1, "the denial should be flagged as an error");
    assert!(tool_result.0.contains("denied"));
}

#[tokio::test]
async fn the_loop_stops_at_the_iteration_limit() {
    // A provider that always asks for a tool would loop forever without the cap.
    let provider = Arc::new(ScriptedProvider::new(
        (0..50)
            .map(|_| Some(("do_thing".into(), json!({}))))
            .collect(),
    ));

    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "do_thing",
        calls: Arc::clone(&calls),
        requires_approval: false,
    });

    let config = AgentConfig {
        max_iterations: 3,
        ..Default::default()
    };

    let agent = Agent::new(provider, Arc::new(registry_with(tool)), config);

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "s", None, tx)
            .await
    });

    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    let (_, _, iterations) = handle.await.unwrap();

    assert_eq!(iterations, 3, "the loop should stop at the limit");
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn tools_are_advertised_to_the_model() {
    let provider = Arc::new(ScriptedProvider::new(vec![None]));
    let tool = Arc::new(RecordingTool {
        name: "do_thing",
        calls: Arc::new(Mutex::new(Vec::new())),
        requires_approval: false,
    });

    let agent = Agent::new(
        provider.clone(),
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("hi")], "s", None, tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    handle.await.unwrap();

    let requests = provider.requests();
    assert_eq!(requests[0].tools.len(), 1);
    assert_eq!(requests[0].tools[0].name, "do_thing");
    assert_eq!(requests[0].tools[0].input_schema["type"], "object");
}

#[tokio::test]
async fn a_disabled_tool_is_not_advertised() {
    let provider = Arc::new(ScriptedProvider::new(vec![None]));
    let tool = Arc::new(RecordingTool {
        name: "do_thing",
        calls: Arc::new(Mutex::new(Vec::new())),
        requires_approval: false,
    });

    let mut registry = registry_with(tool);
    registry.disable("do_thing");

    let agent = Agent::new(provider.clone(), Arc::new(registry), AgentConfig::default());

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("hi")], "s", None, tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    handle.await.unwrap();

    assert!(provider.requests()[0].tools.is_empty());
}

#[tokio::test]
async fn a_provider_error_is_reported_rather_than_panicking() {
    struct BrokenProvider;

    #[async_trait]
    impl Provider for BrokenProvider {
        fn name(&self) -> &str {
            "broken"
        }
        fn model(&self) -> &str {
            "broken"
        }
        async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
            Err(ProviderError::Auth("bad key".into()))
        }
        async fn stream(&self, _request: &CompletionRequest) -> Result<EventStream> {
            Err(ProviderError::Auth("bad key".into()))
        }
    }

    let agent = Agent::new(
        Arc::new(BrokenProvider),
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("hi")], "s", None, tx)
            .await
    });

    let mut saw_error = false;
    while let Some(event) = rx.recv().await {
        if let AgentEvent::Error(message) = &event {
            saw_error = true;
            assert!(message.contains("authentication"));
        }
        if matches!(event, AgentEvent::Error(_) | AgentEvent::Finished { .. }) {
            break;
        }
    }
    let _ = handle.await.unwrap();

    assert!(
        saw_error,
        "the failure should be reported as an error event"
    );
}

#[tokio::test]
async fn a_real_filesystem_tool_works_through_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), "the secret is 7").unwrap();

    let workspace = Workspace::new(dir.path());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new(workspace)));

    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("read_file".into(), json!({ "path": "note.txt" }))),
        None,
    ]));

    let agent = Agent::new(provider.clone(), Arc::new(registry), AgentConfig::default());

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("read it")], "s", None, tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    let (messages, _, _) = handle.await.unwrap();

    // The tool result the model saw contains the file's contents.
    let result = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("a tool result");
    assert!(result.contains("the secret is 7"), "got: {result}");
}

#[tokio::test]
async fn an_approved_tool_runs() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "dangerous",
        calls: Arc::clone(&calls),
        requires_approval: true,
    });

    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("dangerous".into(), json!({ "n": 1 }))),
        None,
    ]));

    let agent = Agent::new(
        provider,
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    let approver = approving_handler();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "s", Some(approver.as_ref()), tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    let (messages, _, _) = handle.await.unwrap();

    assert_eq!(calls.lock().unwrap().len(), 1, "an approved tool must run");

    // The model saw a successful result, not a denial.
    let tool_result = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .expect("a tool result");
    assert!(!tool_result.1, "the result should not be an error");
    assert!(tool_result.0.contains("ran dangerous"));
}

#[tokio::test]
async fn a_tool_the_policy_denies_never_reaches_the_user() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "dangerous",
        calls: Arc::clone(&calls),
        requires_approval: true,
    });

    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("dangerous".into(), json!({}))),
        None,
    ]));

    let config = AgentConfig {
        approval: lia::tool::approval::ApprovalConfig {
            always_denied: vec!["dangerous".into()],
            ..Default::default()
        },
        ..Default::default()
    };

    let agent = Agent::new(provider, Arc::new(registry_with(tool)), config);

    // The approver would say yes, but it must never be asked.
    let approver = approving_handler();

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "s", Some(approver.as_ref()), tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    let (messages, _, _) = handle.await.unwrap();

    assert!(calls.lock().unwrap().is_empty(), "the tool must not run");

    let result = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("a tool result");
    assert!(
        result.contains("approval policy"),
        "the denial should name the policy, got: {result}"
    );
}

#[tokio::test]
async fn approving_always_stops_the_second_prompt() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "dangerous",
        calls: Arc::clone(&calls),
        requires_approval: true,
    });

    // The model calls the same tool twice.
    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("dangerous".into(), json!({ "n": 1 }))),
        Some(("dangerous".into(), json!({ "n": 2 }))),
        None,
    ]));

    let agent = Agent::new(
        provider,
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    let decisions = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&decisions);
    struct AlwaysApprove {
        seen: Arc<Mutex<Vec<(String, ApprovalDecision)>>>,
    }
    #[async_trait]
    impl ApprovalHandler for AlwaysApprove {
        async fn request(
            &self,
            request: lia::tool::approval::ApprovalRequest,
        ) -> Option<ApprovalDecision> {
            self.seen
                .lock()
                .unwrap()
                .push((request.tool.clone(), ApprovalDecision::ApproveAlways));
            Some(ApprovalDecision::ApproveAlways)
        }
    }
    let approver: Arc<dyn ApprovalHandler> = Arc::new(AlwaysApprove { seen: recorder });

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "s", Some(approver.as_ref()), tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    handle.await.unwrap();

    assert_eq!(calls.lock().unwrap().len(), 2, "the tool should run twice");
    assert_eq!(
        decisions.lock().unwrap().len(),
        1,
        "the user should only be asked once"
    );
}

#[tokio::test]
async fn a_session_with_nobody_to_ask_refuses_risky_tools() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "dangerous",
        calls: Arc::clone(&calls),
        requires_approval: true,
    });

    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("dangerous".into(), json!({}))),
        None,
    ]));

    let agent = Agent::new(
        provider,
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    // No approver at all: a headless run must not assume yes.
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "s", None, tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    let (messages, _, _) = handle.await.unwrap();

    assert!(calls.lock().unwrap().is_empty(), "the tool must not run");
    let result = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("a tool result");
    assert!(result.contains("approval policy"), "got: {result}");
}

#[tokio::test]
async fn an_always_allowed_tool_runs_without_asking() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "dangerous",
        calls: Arc::clone(&calls),
        requires_approval: true,
    });

    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("dangerous".into(), json!({}))),
        None,
    ]));

    let config = AgentConfig {
        approval: lia::tool::approval::ApprovalConfig {
            always_allowed: vec!["dangerous".into()],
            ..Default::default()
        },
        ..Default::default()
    };

    let agent = Agent::new(provider, Arc::new(registry_with(tool)), config);

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "s", None, tx)
            .await
    });
    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    handle.await.unwrap();

    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "a standing grant should run the tool without asking"
    );
}

#[tokio::test]
async fn the_spec_declares_the_tools_parameters() {
    // A sanity check that the registry produces usable specs.
    let registry = registry_with(Arc::new(RecordingTool {
        name: "do_thing",
        calls: Arc::new(Mutex::new(Vec::new())),
        requires_approval: false,
    }));

    let specs: Vec<ToolSpec> = registry.specs_for("session");
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "do_thing");
    assert!(!specs[0].description.is_empty());
}

#[tokio::test]
async fn a_tool_that_keeps_failing_ends_the_turn() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(FailingTool {
        name: "do_thing",
        calls: Arc::clone(&calls),
    });

    // The model asks for the same thing, with the same arguments, again
    // and again. The loop should notice and stop.
    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("do_thing".into(), json!({ "x": 1 }))),
        Some(("do_thing".into(), json!({ "x": 1 }))),
        Some(("do_thing".into(), json!({ "x": 1 }))),
        Some(("do_thing".into(), json!({ "x": 1 }))),
        None,
    ]));

    let agent = Agent::new(
        provider.clone(),
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let messages = vec![Message::user("please do the thing")];

    let handle =
        tokio::spawn(async move { agent.run_turn(messages, "test-session", None, tx).await });

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        let done = matches!(event, AgentEvent::Finished { .. });
        events.push(event);
        if done {
            break;
        }
    }
    let (_, _, iterations) = handle.await.unwrap();

    // It stopped after the third identical failure.
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(iterations, 3, "the turn should end with the third failure");

    // And the user was told why.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Notice(message) if message.contains("failed")
        )),
        "the loop was not explained"
    );
}

#[tokio::test]
async fn a_tool_that_works_keeps_the_loop_going() {
    // The same call, but one that succeeds: the loop must not stop.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(RecordingTool {
        name: "do_thing",
        calls: Arc::clone(&calls),
        requires_approval: false,
    });

    let provider = Arc::new(ScriptedProvider::new(vec![
        Some(("do_thing".into(), json!({ "x": 1 }))),
        Some(("do_thing".into(), json!({ "x": 1 }))),
        Some(("do_thing".into(), json!({ "x": 1 }))),
        Some(("do_thing".into(), json!({ "x": 1 }))),
        None,
    ]));

    let agent = Agent::new(
        provider.clone(),
        Arc::new(registry_with(tool)),
        AgentConfig::default(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let handle = tokio::spawn(async move {
        agent
            .run_turn(vec![Message::user("go")], "test-session", None, tx)
            .await
    });

    while let Some(event) = rx.recv().await {
        if matches!(event, AgentEvent::Finished { .. }) {
            break;
        }
    }
    handle.await.unwrap();

    assert_eq!(calls.lock().unwrap().len(), 4);
}
