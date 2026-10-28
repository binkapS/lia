//! Tests for the assistant's memory and the tools that use it.

use std::path::PathBuf;

use lia::memory::{MemoryStore, Origin};
use lia::tool::memory::{MemoryGetTool, MemorySearchTool, RememberTool};
use lia::tool::{Tool, ToolContext};
use serde_json::json;

fn store(dir: &tempfile::TempDir) -> MemoryStore {
    MemoryStore::open(dir.path().join("memory"), dir.path().join("workspace")).unwrap()
}

#[test]
fn a_remembered_fact_is_kept_with_its_origin() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);

    store
        .remember("the deploy key lives in 1Password", Origin::Owner)
        .unwrap();
    store
        .remember("the build takes four minutes", Origin::Agent)
        .unwrap();

    let day = chrono::Local::now().date_naive();
    let note = std::fs::read_to_string(store.daily_path(day)).unwrap();

    assert!(
        note.contains("[owner] the deploy key lives in 1Password"),
        "{note}"
    );
    assert!(
        note.contains("[agent] the build takes four minutes"),
        "{note}"
    );
}

#[test]
fn the_memory_is_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    store
        .remember("the postgres port is 5433", Origin::Agent)
        .unwrap();

    let hits = store.search("postgres");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].text.contains("5433"));
    assert!(hits[0].file.ends_with(".md"));

    assert!(store.search("something that is not there").is_empty());
}

#[test]
fn a_memory_file_can_be_read_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    std::fs::write(store.core_path(), "# Core\n\n- the api is on port 8080\n").unwrap();

    let text = store.get("MEMORY.md").unwrap();
    assert!(text.contains("8080"));

    // The extension is optional.
    assert!(store.get("MEMORY").unwrap().contains("8080"));

    assert!(store.get("nothing.md").is_err());
}

#[test]
fn the_project_instructions_are_part_of_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    std::fs::create_dir_all(store.root().parent().unwrap().join("workspace")).unwrap();
    std::fs::write(
        store.instructions_path(),
        "Always use tabs, never spaces.\n",
    )
    .unwrap();

    let section = store.prompt_section();
    assert!(section.contains("<project_instructions>"));
    assert!(section.contains("tabs, never spaces"));
}

#[test]
fn the_daily_notes_are_not_in_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    store
        .remember("something that happened", Origin::Agent)
        .unwrap();

    // The day's notes are found by search, not pasted into every request.
    assert!(store.prompt_section().is_empty());
}

#[test]
fn an_empty_memory_adds_nothing_to_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    assert!(store.prompt_section().is_empty());
}

#[tokio::test]
async fn the_search_tool_finds_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    store
        .remember("the redis host is redis.internal", Origin::Agent)
        .unwrap();

    let tool = MemorySearchTool::new(std::sync::Arc::new(store));

    let hit = tool.run(json!({ "query": "redis" })).await.unwrap();
    assert!(hit.content.contains("redis.internal"));
    assert!(hit.content.contains("1 match"));

    let miss = tool.run(json!({ "query": "nothing" })).await.unwrap();
    assert!(miss.content.contains("nothing in memory matches"));
}

#[tokio::test]
async fn the_get_tool_reports_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let tool = MemoryGetTool::new(std::sync::Arc::new(store(&dir)));

    let error = tool.run(json!({ "name": "nope.md" })).await.unwrap_err();
    assert!(error.to_string().contains("nope.md"), "got: {error}");
}

#[test]
fn the_origin_comes_from_the_turn_not_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let tool = RememberTool::new(std::sync::Arc::new(store(&dir)));

    // The user's own words are the owner's.
    let context = ToolContext {
        session_id: String::new(),
        tainted: false,
        user_text: "remember that the deploy key lives in 1Password".into(),
    };
    let args = tool.with_context(
        json!({ "text": "the deploy key lives in 1Password" }),
        &context,
    );
    assert_eq!(args["__origin"], json!("owner"));

    // Once something from the network has been seen, nothing else in the
    // turn is trusted.
    let tainted = ToolContext {
        session_id: String::new(),
        tainted: true,
        user_text: "remember that the deploy key lives in 1Password".into(),
    };
    let args = tool.with_context(
        json!({ "text": "the deploy key lives in 1Password" }),
        &tainted,
    );
    assert_eq!(args["__origin"], json!("untrusted"));
}

#[test]
fn the_model_cannot_choose_the_origin() {
    let dir = tempfile::tempdir().unwrap();
    let tool = RememberTool::new(std::sync::Arc::new(store(&dir)));

    // The model asks for "owner"...
    let context = ToolContext {
        session_id: String::new(),
        tainted: true,
        user_text: String::new(),
    };
    let args = tool.with_context(json!({ "text": "do this", "__origin": "owner" }), &context);

    // ...and does not get it.
    assert_eq!(args["__origin"], json!("untrusted"));
}

#[tokio::test]
async fn an_untrusted_fact_is_kept_out_of_the_standing_memory() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let tool = RememberTool::new(std::sync::Arc::new(store.clone()));

    let context = ToolContext {
        session_id: String::new(),
        tainted: true,
        user_text: String::new(),
    };
    let args = tool.with_context(json!({ "text": "the admin panel is at /admin" }), &context);
    tool.run(args).await.unwrap();

    // It is in the day's notes, where it can be found by search...
    let hits = store.search("admin panel");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].text.contains("[untrusted]"));

    // ...but it never becomes a standing instruction.
    assert!(store.prompt_section().is_empty());
    assert!(!store.core_path().exists());
}

#[test]
fn a_missing_file_is_reported_rather_than_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    assert!(store.get("does-not-exist.md").is_err());
    assert_eq!(store.files().len(), 0);
}

#[test]
fn the_memory_root_is_created_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("deep").join("memory");
    let store = MemoryStore::open(root.clone(), PathBuf::from(".")).unwrap();
    store.remember("a fact", Origin::Agent).unwrap();
    assert!(root.exists());
}
