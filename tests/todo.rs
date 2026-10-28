//! Tests of the list of things the assistant is working through.

use lia::tool::todo::{TodoItem, TodoStatus, TodoTool};
use lia::tool::{Tool, ToolContext};
use serde_json::json;

/// The list as the model writes it.
fn a_list() -> serde_json::Value {
    json!({
        "action": "write",
        "todos": [
            { "content": "read the file", "status": "done" },
            { "content": "fix the bug", "status": "in_progress" },
            { "content": "run the tests", "status": "pending" }
        ]
    })
}

/// The tool, with the session the turn belongs to.
async fn call(tool: &TodoTool, session: &str, args: serde_json::Value) -> lia::tool::ToolOutput {
    let context = ToolContext {
        session_id: session.into(),
        ..Default::default()
    };
    let args = tool.with_context(args, &context);
    tool.run(args).await.expect("the tool works")
}

#[tokio::test]
async fn a_list_is_written_and_read_back() {
    let tool = TodoTool::new();

    let output = call(&tool, "chat:one", a_list()).await;
    assert!(!output.is_error, "{}", output.content);
    assert!(output.content.contains("fix the bug"));

    let output = call(&tool, "chat:one", json!({ "action": "read" })).await;
    assert!(
        output.content.contains("[+] read the file"),
        "{}",
        output.content
    );
    assert!(
        output.content.contains("[>] fix the bug"),
        "{}",
        output.content
    );
    assert!(
        output.content.contains("[ ] run the tests"),
        "{}",
        output.content
    );
}

#[tokio::test]
async fn each_conversation_has_its_own_list() {
    let tool = TodoTool::new();
    call(&tool, "chat:one", a_list()).await;

    let other = call(&tool, "chat:two", json!({ "action": "read" })).await;
    assert!(
        other.content.contains("the list is empty"),
        "one chat saw another's list: {}",
        other.content
    );

    let first = call(&tool, "chat:one", json!({ "action": "read" })).await;
    assert!(first.content.contains("fix the bug"));
}

#[tokio::test]
async fn the_model_cannot_write_into_another_conversation() {
    let tool = TodoTool::new();

    // The model tries to name the conversation itself.
    let args = json!({
        "action": "write",
        "todos": [{ "content": "sneaky", "status": "pending" }],
        "__session": "chat:somebody-else"
    });
    let output = call(&tool, "chat:mine", args).await;
    assert!(!output.is_error);

    // It landed in the conversation the turn belonged to.
    let mine = call(&tool, "chat:mine", json!({ "action": "read" })).await;
    assert!(mine.content.contains("sneaky"), "{}", mine.content);

    let theirs = call(&tool, "chat:somebody-else", json!({ "action": "read" })).await;
    assert!(
        theirs.content.contains("the list is empty"),
        "the model wrote into another conversation: {}",
        theirs.content
    );
}

#[tokio::test]
async fn a_list_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();

    {
        let tool = TodoTool::with_dir(dir.path()).unwrap();
        call(&tool, "chat:one", a_list()).await;
    }

    // A new process, the same state directory.
    let tool = TodoTool::with_dir(dir.path()).unwrap();
    let output = call(&tool, "chat:one", json!({ "action": "read" })).await;
    assert!(output.content.contains("fix the bug"), "{}", output.content);
}

#[tokio::test]
async fn a_list_can_be_cleared() {
    let tool = TodoTool::new();
    call(&tool, "chat:one", a_list()).await;

    let output = call(&tool, "chat:one", json!({ "action": "clear" })).await;
    assert!(output.content.contains("empty"), "{}", output.content);

    let output = call(&tool, "chat:one", json!({ "action": "read" })).await;
    assert!(
        output.content.contains("the list is empty"),
        "{}",
        output.content
    );
}

#[tokio::test]
async fn the_list_is_told_to_the_interface() {
    let tool = TodoTool::new();
    let output = call(&tool, "chat:one", a_list()).await;

    // The interface shows the plan, so the tool says what the plan is.
    let metadata = output.metadata.expect("the tool says what the plan is");
    let todos: Vec<TodoItem> = serde_json::from_value(metadata["todos"].clone()).unwrap();
    assert_eq!(todos.len(), 3);
    assert_eq!(todos[1].status, TodoStatus::InProgress);
    assert_eq!(metadata["remaining"], 2);
}

#[tokio::test]
async fn the_interface_shows_the_plan() {
    use lia::agent::AgentEvent;
    use lia::tui::App;

    let tool = TodoTool::new();
    let output = call(&tool, "chat:one", a_list()).await;

    let mut app = App::new(lia::Config::default());
    app.apply_event(AgentEvent::ToolCallFinished {
        id: "call-1".into(),
        name: "todo".into(),
        output,
    })
    .await;

    assert_eq!(app.plan.len(), 3);
    assert_eq!(app.plan[0].content, "read the file");
    assert_eq!(app.plan[1].status, TodoStatus::InProgress);
}

#[tokio::test]
async fn the_plan_is_drawn() {
    use lia::agent::AgentEvent;
    use lia::tui::{App, render};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let tool = TodoTool::new();
    let output = call(&tool, "chat:one", a_list()).await;

    let mut app = App::new(lia::Config::default());
    app.apply_event(AgentEvent::ToolCallFinished {
        id: "call-1".into(),
        name: "todo".into(),
        output,
    })
    .await;

    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    render::draw(&mut terminal, &mut app).unwrap();

    let buffer = terminal.backend().buffer();
    let mut screen = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            screen.push_str(buffer[(x, y)].symbol());
        }
        screen.push('\n');
    }
    assert!(
        screen.contains("fix the bug"),
        "the plan was not drawn:\n{screen}"
    );
    assert!(
        screen.contains("1/3 done"),
        "the plan had no count:\n{screen}"
    );
}
