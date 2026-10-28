//! Tests of handing a piece of work to another assistant.
//!
//! The other assistant works in a conversation of its own, with the same
//! tools and the same rules, and answers with the result.

mod common;

use std::sync::{Arc, Mutex};

use lia::agent::session::SessionStore;
use lia::agent::{Agent, AgentConfig, AgentEvent, SessionRunner};
use lia::tool::ToolRegistry;
use lia::tool::task::{TaskRunner, TaskRunnerHandle, TaskTool};
use lia::tool::{Tool, ToolContext};
use serde_json::json;

/// A runner that records what it was asked to do, and answers with a
/// fixed reply.
struct FakeRunner {
    calls: Mutex<Vec<(String, String)>>,
    answer: String,
}

impl FakeRunner {
    fn new(answer: &str) -> Arc<FakeRunner> {
        Arc::new(FakeRunner {
            calls: Mutex::new(Vec::new()),
            answer: answer.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl TaskRunner for FakeRunner {
    async fn run_task(
        &self,
        session_id: &str,
        prompt: &str,
        _label: &str,
    ) -> lia::tool::Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push((session_id.to_string(), prompt.to_string()));
        Ok(self.answer.clone())
    }
}

/// The tool, with somewhere to run the work.
///
/// The strong handle is returned as well: the tool holds a weak one, so
/// somebody has to keep the runner alive.
fn tool_with(runner: Arc<FakeRunner>) -> (TaskTool, Arc<dyn TaskRunner>) {
    let tool = TaskTool::new();
    let as_runner: Arc<dyn TaskRunner> = runner;
    tool.set_task_runner(TaskRunnerHandle::new(&as_runner));
    (tool, as_runner)
}

#[tokio::test]
async fn a_task_is_run_in_the_conversation_that_asked_for_it() {
    let runner = FakeRunner::new("the answer is 42");
    let (tool, _runner) = tool_with(runner.clone());

    let context = ToolContext {
        session_id: "telegram:12345".into(),
        ..Default::default()
    };
    let args = tool.with_context(json!({ "prompt": "count the files in the repo" }), &context);
    let output = tool.run(args).await.expect("the task runs");

    assert!(!output.is_error, "{}", output.content);
    assert_eq!(output.content, "the answer is 42");

    // The work belonged to the conversation that asked for it.
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "telegram:12345");
    assert!(
        calls[0].1.contains("count the files in the repo"),
        "the brief was lost: {}",
        calls[0].1
    );
}

#[tokio::test]
async fn the_model_cannot_choose_whose_task_it_is() {
    let runner = FakeRunner::new("the answer");
    let (tool, _runner) = tool_with(runner.clone());

    // The model tries to name the conversation itself.
    let context = ToolContext {
        session_id: "telegram:mine".into(),
        ..Default::default()
    };
    let args = tool.with_context(
        json!({ "prompt": "do it", "__session": "telegram:somebody-else" }),
        &context,
    );
    tool.run(args).await.expect("the task runs");

    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].0, "telegram:mine");
}

#[tokio::test]
async fn a_task_needs_something_to_do() {
    let (tool, _runner) = tool_with(FakeRunner::new("the answer"));
    let error = tool.run(json!({})).await.unwrap_err();
    assert!(
        error.to_string().contains("prompt"),
        "the error should name what is missing: {error}"
    );
}

#[tokio::test]
async fn without_a_runner_there_is_nowhere_to_run_a_task() {
    let tool = TaskTool::new();
    let error = tool.run(json!({ "prompt": "do it" })).await.unwrap_err();
    assert!(
        error.to_string().contains("nowhere"),
        "the error should say what is wrong: {error}"
    );
}

#[tokio::test]
async fn a_task_cannot_be_handed_on_again() {
    let runner = FakeRunner::new("the answer");
    let (tool, _runner) = tool_with(runner.clone());

    // A sub-assistant cannot hand work to another one.
    let context = ToolContext {
        session_id: "sub:1234".into(),
        ..Default::default()
    };
    let args = tool.with_context(json!({ "prompt": "do it" }), &context);
    let error = tool.run(args).await.unwrap_err();
    assert!(
        error.to_string().contains("already working"),
        "the error should say what is wrong: {error}"
    );

    // And the tool is not even offered to it.
    assert!(!tool.is_enabled("sub:1234"));
    assert!(tool.is_enabled("telegram:1234"));
}

/// The whole thing, with the model asking for the work.
#[tokio::test]
async fn the_assistant_hands_work_over_and_gets_the_answer_back() {
    let provider = Arc::new(common::MockProvider::with_tool(
        "task",
        json!({ "prompt": "look the thing up" }),
    ));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TaskTool::new()));

    let agent = Agent::new(provider.clone(), Arc::new(registry), AgentConfig::default());
    let runner = SessionRunner::new(
        Arc::new(agent),
        Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let session = "cli";
    runner.run(session, Some("find out".into()), tx).await;

    // The answer came back through the tool.
    let mut tool_result = None;
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::ToolCallFinished { name, output, .. } = event
            && name == "task"
        {
            tool_result = Some(output);
        }
    }
    let output = tool_result.expect("the task tool ran");
    assert_eq!(output.content, "the answer");

    // The other assistant started with a clean context: the brief, and
    // nothing of this conversation.
    let seen = provider.seen();
    assert!(seen.len() >= 2, "the other assistant was never asked");
    let brief = &seen[1].messages;
    assert_eq!(brief.len(), 1, "the other assistant saw this conversation");
    assert!(
        brief[0].text().contains("look the thing up"),
        "the brief was lost: {}",
        brief[0].text()
    );

    // And it was not offered the tool that hands work over.
    let offered: Vec<&str> = seen[1]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert!(
        !offered.contains(&"task"),
        "the work could nest: {offered:?}"
    );
}

#[tokio::test]
async fn what_another_assistant_says_is_treated_as_data() {
    let runner = FakeRunner::new("the answer");
    let (tool, _runner) = tool_with(runner);

    let context = ToolContext {
        session_id: "cli".into(),
        ..Default::default()
    };
    let args = tool.with_context(json!({ "prompt": "do it" }), &context);
    let output = tool.run(args).await.expect("the task runs");

    // The answer came from another assistant, which may have read
    // anything, so the turn that used it is tainted.
    let metadata = output
        .metadata
        .expect("the tool says where the answer came from");
    assert_eq!(metadata["source"], "assistant");
}

/// An approver that says yes, and remembers what it was asked.
struct Yes {
    asked: Mutex<Vec<lia::tool::approval::ApprovalRequest>>,
}

#[async_trait::async_trait]
impl lia::agent::ApprovalHandler for Yes {
    async fn request(
        &self,
        request: lia::tool::approval::ApprovalRequest,
    ) -> Option<lia::tool::approval::ApprovalDecision> {
        self.asked.lock().unwrap().push(request);
        Some(lia::tool::approval::ApprovalDecision::Approve)
    }
}

#[tokio::test]
async fn the_other_works_risky_tools_are_answered_by_the_same_person() {
    // The parent hands the work over; the other assistant then wants to
    // run something that needs permission.
    let provider = Arc::new(common::MockProvider::with_tools(vec![
        ("task".to_string(), json!({ "prompt": "do the thing" })),
        ("do_thing".to_string(), json!({ "x": 1 })),
    ]));

    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(common::RecordingTool {
        name: "do_thing",
        calls: calls.clone(),
        requires_approval: true,
    });

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TaskTool::new()));
    registry.register(tool);

    let agent = Agent::new(provider, Arc::new(registry), AgentConfig::default());
    let runner = SessionRunner::new(
        Arc::new(agent),
        Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
    );

    // The person talking to the main assistant is the one who answers.
    let approver = Arc::new(Yes {
        asked: Mutex::new(Vec::new()),
    });
    runner.set_approver("cli", approver.clone()).await;

    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    runner.run("cli", Some("find out".into()), tx).await;

    // The other assistant's risky tool was run, and the person who was
    // asked is the one who was talking to the main assistant.
    assert_eq!(calls.lock().unwrap().len(), 1, "the tool did not run");
    let asked = approver.asked.lock().unwrap();
    assert_eq!(asked.len(), 1, "nobody was asked for permission");
    assert_eq!(asked[0].tool, "do_thing");
}
