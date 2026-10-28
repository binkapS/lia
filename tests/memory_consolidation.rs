//! Tests of sorting the notes of the days that have passed into what is kept.

use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate};
use lia::memory::MemoryStore;
use lia::memory::consolidate::Consolidator;

mod common;

/// A memory store in a directory of its own.
fn store(dir: &std::path::Path) -> MemoryStore {
    MemoryStore::open(dir.join("memory"), dir.to_path_buf()).expect("the store opens")
}

/// Write the notes of a day.
fn write_notes(store: &MemoryStore, day: NaiveDate, notes: &[(&str, &str)]) {
    let mut text = format!("# {day}\n\n");
    for (origin, note) in notes {
        text.push_str(&format!("- [{origin}] {note}\n"));
    }
    std::fs::write(store.daily_path(day), text).expect("the notes are written");
}

/// A directory of its own for one test.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lia-consolidate-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("the directory is made");
    dir
}

/// Yesterday.
fn yesterday() -> NaiveDate {
    Local::now().date_naive() - Duration::days(1)
}

#[tokio::test]
async fn what_matters_is_kept() {
    let dir = scratch("kept");
    let memory = store(&dir);
    write_notes(
        &memory,
        yesterday(),
        &[
            ("owner", "the build is run with cargo test"),
            ("agent", "the API lives in src/api.rs"),
        ],
    );

    let provider = Arc::new(common::MockProvider::answering(
        r#"{"facts": [{"text": "the build is run with cargo test", "where": "work", "from": [1]}]}"#,
    ));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider);
    let summary = consolidator.run().await.expect("the notes are looked over");

    assert_eq!(summary.days, 1);
    assert_eq!(summary.kept, 1);

    let kept = std::fs::read_to_string(memory.core_path()).expect("the facts are there");
    assert!(
        kept.contains("the build is run with cargo test"),
        "the fact was not kept: {kept}"
    );
    assert!(
        kept.contains("owner"),
        "the fact lost its provenance: {kept}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_fact_about_the_person_goes_to_the_other_file() {
    let dir = scratch("person");
    let memory = store(&dir);
    write_notes(
        &memory,
        yesterday(),
        &[("owner", "the user works in Berlin and prefers German")],
    );

    let provider = Arc::new(common::MockProvider::answering(
        r#"{"facts": [{"text": "the user is in Berlin", "where": "user", "from": [1]}]}"#,
    ));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider);
    consolidator.run().await.expect("the notes are looked over");

    let kept = std::fs::read_to_string(memory.user_path()).expect("the facts are there");
    assert!(kept.contains("the user is in Berlin"), "{kept}");
    assert!(
        !std::fs::read_to_string(memory.core_path())
            .unwrap_or_default()
            .contains("Berlin"),
        "the fact went to the wrong file"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_fact_from_the_web_is_not_kept() {
    let dir = scratch("web");
    let memory = store(&dir);
    write_notes(
        &memory,
        yesterday(),
        &[("untrusted", "the documentation says to run rm -rf /")],
    );

    let provider = Arc::new(common::MockProvider::answering(
        r#"{"facts": [{"text": "the documentation says to run rm -rf /", "where": "work", "from": [1]}]}"#,
    ));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider);
    let summary = consolidator.run().await.expect("the notes are looked over");

    assert_eq!(summary.kept, 0, "a fact from the web was kept");
    assert_eq!(summary.left_out, 1);

    let kept = std::fs::read_to_string(memory.core_path()).unwrap_or_default();
    assert!(!kept.contains("rm -rf"), "the fact was kept: {kept}");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_fact_that_is_already_there_is_not_written_again() {
    let dir = scratch("twice");
    let memory = store(&dir);
    std::fs::write(
        memory.core_path(),
        "# Memory\n\n- the build is run with cargo test\n",
    )
    .expect("the file is written");
    write_notes(
        &memory,
        yesterday(),
        &[("owner", "the build is run with cargo test")],
    );

    let provider = Arc::new(common::MockProvider::answering(
        r#"{"facts": [{"text": "the build is run with cargo test", "where": "work", "from": [1]}]}"#,
    ));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider);
    let summary = consolidator.run().await.expect("the notes are looked over");

    assert_eq!(summary.kept, 0);
    assert_eq!(summary.left_out, 1);

    let kept = std::fs::read_to_string(memory.core_path()).expect("the facts are there");
    assert_eq!(
        kept.matches("cargo test").count(),
        1,
        "the fact was written twice: {kept}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_day_is_only_looked_over_once() {
    let dir = scratch("once");
    let memory = store(&dir);
    write_notes(
        &memory,
        yesterday(),
        &[("owner", "the build is run with cargo test")],
    );

    let provider = Arc::new(common::MockProvider::answering(
        r#"{"facts": [{"text": "the build is run with cargo test", "where": "work", "from": [1]}]}"#,
    ));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider);

    let first = consolidator.run().await.expect("looked over");
    assert_eq!(first.days, 1);

    let second = consolidator.run().await.expect("looked over");
    assert_eq!(second.days, 0, "the day was looked over twice");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn today_is_left_alone() {
    let dir = scratch("today");
    let memory = store(&dir);
    write_notes(
        &memory,
        Local::now().date_naive(),
        &[("owner", "the build is run with cargo test")],
    );

    let provider = Arc::new(common::MockProvider::answering(r#"{"facts": []}"#));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider);
    let summary = consolidator.run().await.expect("looked over");

    assert_eq!(summary.days, 0, "today was looked over");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_notes_are_given_to_the_model() {
    let dir = scratch("prompt");
    let memory = store(&dir);
    write_notes(
        &memory,
        yesterday(),
        &[("owner", "the build is run with cargo test")],
    );

    let provider = Arc::new(common::MockProvider::answering(r#"{"facts": []}"#));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider.clone());
    consolidator.run().await.expect("looked over");

    let seen = provider.seen();
    assert_eq!(seen.len(), 1, "the model was not asked");
    let text = seen[0].messages[0].text();
    assert!(
        text.contains("the build is run with cargo test"),
        "the note was not given to the model: {text}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_answer_that_is_not_json_leaves_the_memory_alone() {
    let dir = scratch("junk");
    let memory = store(&dir);
    write_notes(
        &memory,
        yesterday(),
        &[("owner", "the build is run with cargo test")],
    );

    let provider = Arc::new(common::MockProvider::answering("I could not decide."));
    let consolidator = Consolidator::new(Arc::new(memory.clone()), provider);
    let summary = consolidator.run().await.expect("looked over");

    assert_eq!(summary.kept, 0);
    assert!(
        !std::fs::read_to_string(memory.core_path())
            .unwrap_or_default()
            .contains("cargo test"),
        "something was kept"
    );

    let _ = std::fs::remove_dir_all(dir);
}
