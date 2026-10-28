//! Tests for the tool implementations.

use lia::tool::Tool;
use lia::tool::filesystem::{
    EditFileTool, ListDirectoryTool, ReadFileTool, SearchTool, Workspace, WriteFileTool,
};
use lia::tool::search::WebSearchTool;
use serde_json::json;

/// A workspace in a temporary directory.
fn workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(dir.path());
    (dir, workspace)
}

#[tokio::test]
async fn read_file_returns_numbered_lines() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();

    let tool = ReadFileTool::new(ws);
    let output = tool
        .run(json!({ "path": "a.txt" }))
        .await
        .expect("read succeeds");

    assert!(!output.is_error);
    assert!(output.content.contains("1\tonetwo") || output.content.contains("1\tone"));
    assert!(output.content.contains("3\tthree"));
}

#[tokio::test]
async fn read_file_honours_offset_and_limit() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();

    let tool = ReadFileTool::new(ws);
    let output = tool
        .run(json!({ "path": "a.txt", "offset": 2, "limit": 2 }))
        .await
        .expect("read succeeds");

    assert!(output.content.contains("two"));
    assert!(output.content.contains("three"));
    assert!(!output.content.contains("four"));
}

#[tokio::test]
async fn read_file_rejects_paths_outside_the_workspace() {
    let (_dir, ws) = workspace();
    let tool = ReadFileTool::new(ws);

    // A relative escape.
    let result = tool.run(json!({ "path": "../../etc/passwd" })).await;
    assert!(result.is_err(), "path escape must be rejected");

    // An absolute path outside the root.
    let result = tool.run(json!({ "path": "/etc/passwd" })).await;
    assert!(result.is_err(), "absolute escape must be rejected");
}

#[tokio::test]
async fn write_then_read_round_trips() {
    let (dir, ws) = workspace();
    let tool = WriteFileTool::new(ws.clone());

    let output = tool
        .run(json!({ "path": "sub/dir/new.txt", "content": "hello" }))
        .await
        .expect("write succeeds");
    assert!(!output.is_error);
    assert!(dir.path().join("sub/dir/new.txt").exists());

    let reader = ReadFileTool::new(ws);
    let output = reader
        .run(json!({ "path": "sub/dir/new.txt" }))
        .await
        .unwrap();
    assert!(output.content.contains("hello"));
}

#[tokio::test]
async fn edit_file_replaces_a_unique_string() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.txt"), "alpha beta gamma").unwrap();

    let tool = EditFileTool::new(ws);
    let output = tool
        .run(json!({ "path": "a.txt", "old_string": "beta", "new_string": "BETA" }))
        .await
        .expect("edit succeeds");

    assert!(!output.is_error);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "alpha BETA gamma"
    );
}

#[tokio::test]
async fn edit_file_refuses_an_ambiguous_match() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.txt"), "x x x").unwrap();

    let tool = EditFileTool::new(ws);
    let result = tool
        .run(json!({ "path": "a.txt", "old_string": "x", "new_string": "y" }))
        .await;

    assert!(result.is_err(), "ambiguous match must be refused");
    // The file must be untouched.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "x x x"
    );
}

#[tokio::test]
async fn edit_file_replaces_all_when_asked() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.txt"), "x x x").unwrap();

    let tool = EditFileTool::new(ws);
    tool.run(json!({
        "path": "a.txt",
        "old_string": "x",
        "new_string": "y",
        "replace_all": true
    }))
    .await
    .expect("edit succeeds");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "y y y"
    );
}

#[tokio::test]
async fn list_directory_shows_entries() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();

    let tool = ListDirectoryTool::new(ws);
    let output = tool.run(json!({})).await.expect("list succeeds");

    assert!(output.content.contains("a.txt"));
    assert!(output.content.contains("sub"));
}

#[tokio::test]
async fn search_finds_files_by_glob() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.path().join("b.txt"), "hello").unwrap();

    let tool = SearchTool::new(ws);
    let output = tool
        .run(json!({ "pattern": "**/*.rs" }))
        .await
        .expect("search succeeds");

    assert!(output.content.contains("a.rs"));
    assert!(!output.content.contains("b.txt"));
}

#[tokio::test]
async fn search_finds_content_by_regex() {
    let (dir, ws) = workspace();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\nlet x = 1;").unwrap();

    let tool = SearchTool::new(ws);
    let output = tool
        .run(json!({ "content": "let x" }))
        .await
        .expect("search succeeds");

    assert!(output.content.contains("a.rs"));
    assert!(output.content.contains("let x"));
}

#[tokio::test]
async fn missing_parameters_are_reported_clearly() {
    let (_dir, ws) = workspace();
    let tool = ReadFileTool::new(ws);

    let error = tool.run(json!({})).await.unwrap_err();
    assert!(
        error.to_string().contains("path"),
        "error should name the missing parameter: {error}"
    );
}

/// A search engine to test against: a server that answers every question
/// with the same page.
async fn search_engine(body: &'static str, content_type: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let app = axum::Router::new().fallback(move || async move {
        ([(axum::http::header::CONTENT_TYPE, content_type)], body)
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{address}")
}

/// A page of search results, as an engine would return it.
const RESULTS_PAGE: &str = r##"
    <div class="result">
      <a rel="nofollow" class="result__a"
         href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fguide">The Rust guide</a>
      <a class="result__snippet" href="#">Everything about Rust.</a>
    </div>
    <div class="result">
      <a rel="nofollow" class="result__a" href="https://example.org/rust">The Rust book</a>
      <a class="result__snippet" href="#">A book about Rust.</a>
    </div>
"##;

#[tokio::test]
async fn a_search_returns_the_results_it_found() {
    let engine = search_engine(RESULTS_PAGE, "text/html").await;
    let tool = WebSearchTool::new().with_html_endpoint(format!("{engine}/html/"));

    let output = tool
        .run(json!({ "query": "rust" }))
        .await
        .expect("search succeeds");

    assert!(!output.is_error);
    assert!(
        output.content.contains("The Rust guide"),
        "{}",
        output.content
    );
    assert!(
        output.content.contains("https://example.com/guide"),
        "{}",
        output.content
    );
    assert!(output.content.contains("The Rust book"));
}

#[tokio::test]
async fn a_search_of_a_json_engine_is_read() {
    let body = r#"{"results":[
        {"title":"The Rust guide","url":"https://example.com/guide","content":"Everything about Rust."}
    ]}"#;
    let engine = search_engine(body, "application/json").await;
    let tool = WebSearchTool::new().with_json_endpoint(format!("{engine}/search"));

    let output = tool
        .run(json!({ "query": "rust" }))
        .await
        .expect("search succeeds");

    assert!(
        output.content.contains("The Rust guide"),
        "{}",
        output.content
    );
    assert!(output.content.contains("https://example.com/guide"));
}

#[tokio::test]
async fn a_search_with_nothing_to_show_says_so() {
    let engine = search_engine("<html><body>nothing here</body></html>", "text/html").await;
    let tool = WebSearchTool::new().with_html_endpoint(format!("{engine}/html/"));

    let output = tool
        .run(json!({ "query": "rust" }))
        .await
        .expect("search succeeds");

    assert!(output.content.contains("no results"), "{}", output.content);
}

#[tokio::test]
async fn a_search_needs_something_to_search_for() {
    let tool = WebSearchTool::new();
    let error = tool.run(json!({})).await.unwrap_err();
    assert!(
        error.to_string().contains("query"),
        "error should name the missing parameter: {error}"
    );
}

#[tokio::test]
async fn what_a_search_finds_is_treated_as_data() {
    let engine = search_engine(RESULTS_PAGE, "text/html").await;
    let tool = WebSearchTool::new().with_html_endpoint(format!("{engine}/html/"));

    let output = tool
        .run(json!({ "query": "rust" }))
        .await
        .expect("search succeeds");

    // The agent taints the turn when a tool says the answer came from the
    // network, so nothing found on the web can become an instruction.
    let metadata = output
        .metadata
        .expect("the tool says where the results came from");
    assert_eq!(metadata["source"], "network");
}

#[tokio::test]
async fn the_system_health_tool_reads_the_machine() {
    use lia::tool::system::SystemHealthTool;

    let output = SystemHealthTool
        .run(json!({ "detail": "full", "sample_ms": 10 }))
        .await
        .expect("the machine can be read");

    assert!(!output.is_error);
    assert!(output.content.contains("# System health"));
    assert!(output.content.contains("CPU:"));
    assert!(output.content.contains("Memory:"));

    let metadata = output.metadata.expect("the reading is structured");
    assert!(metadata["health"]["score"].as_u64().unwrap() <= 100);
    assert!(!metadata["health"]["checks"].as_array().unwrap().is_empty());
    assert!(metadata["cpu_percent"].as_f64().is_some());
    assert!(metadata["memory_percent"].as_f64().is_some());
}

#[tokio::test]
async fn the_process_tool_honours_its_limit() {
    use lia::tool::system::ProcessListTool;

    let output = ProcessListTool
        .run(json!({ "limit": 3, "sample_ms": 10 }))
        .await
        .expect("the process table can be read");

    assert!(!output.is_error);
    let metadata = output.metadata.expect("the list is structured");
    assert!(metadata["processes"].as_array().unwrap().len() <= 3);
}

#[tokio::test]
async fn the_security_tool_never_prints_the_token() {
    use lia::config::Config;
    use lia::tool::system::{SecurityFacts, SecurityStatusTool};

    let mut config = Config::default();
    config.gateway.token = Some("a-token-that-must-not-leak".into());

    let output = SecurityStatusTool::new(SecurityFacts::from_config(&config))
        .run(json!({}))
        .await
        .expect("the posture can be read");

    assert!(!output.is_error);
    assert!(
        !output.content.contains("a-token-that-must-not-leak"),
        "the token must never appear in a tool result"
    );
    assert!(output.content.contains("authentication"));
}
