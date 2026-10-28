//! Tests of the example configuration file.
//!
//! The example is what most people copy, so it has to be right. The trap
//! is TOML's rule that a key written after a section header belongs to
//! that section: a setting that belongs to the top level has to come
//! before any header, or it is quietly filed under the wrong section and
//! never read.

use lia::config::{Config, ThemeKind};
use lia::tool::approval::ApprovalPolicy;

/// The example file, read from the crate's root.
fn example() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/lia.example.toml");
    std::fs::read_to_string(path).expect("the example file is there")
}

#[test]
fn the_example_file_is_read_as_it_looks() {
    let config: Config = toml::from_str(&example()).expect("the example file makes sense");

    // The plain settings.
    assert_eq!(config.provider.as_str(), "openai");
    assert_eq!(config.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(config.effective_model(), "gpt-4o-mini");
    assert_eq!(config.max_output_tokens, 4096);
    assert_eq!(config.max_iterations, 25);
    assert!(config.tools_enabled);
    assert!(
        config.skills_enabled,
        "the skill setting was filed elsewhere"
    );
    assert!(config.mcp_enabled, "the MCP setting was filed elsewhere");
    assert_eq!(config.log_level, "info");
    assert_eq!(config.theme, ThemeKind::Auto);

    // The sections.
    assert_eq!(config.approval.policy, ApprovalPolicy::Ask);
    assert!(!config.sandbox.enabled);
    assert!(config.sandbox.network);
    assert!(!config.sandbox.hide_home);
    assert!(config.memory.consolidate);
    assert_eq!(config.memory.every_hours, 12);
    assert_eq!(config.search.count, 5);
    assert!(config.voice.enabled);
    assert_eq!(config.voice.max_seconds, 60);
    assert_eq!(config.gateway.host, "127.0.0.1");
    assert_eq!(config.gateway.port, 8787);
    assert!(config.filesystem.allow.is_empty());
    assert!(!config.filesystem.read_only);
}

#[test]
fn the_folders_of_the_file_server_are_read() {
    let config: Config = toml::from_str(
        "[filesystem]\nallow = [\"/home/you/projects\", \"~/notes\"]\nread_only = true",
    )
    .expect("the setting makes sense");
    assert_eq!(
        config.filesystem.allow,
        vec![
            std::path::PathBuf::from("/home/you/projects"),
            std::path::PathBuf::from("~/notes"),
        ]
    );
    assert!(config.filesystem.read_only);
}

#[test]
fn the_example_file_says_how_to_retry() {
    let config: Config = toml::from_str(&example()).expect("the example file makes sense");
    assert_eq!(config.retry.attempts, 3);
    assert_eq!(config.retry.backoff_ms, 500);
}

#[test]
fn a_provider_without_a_model_gets_its_own() {
    let config: Config = toml::from_str("provider = \"ollama\"").expect("the setting makes sense");
    assert_eq!(config.model, None);
    assert_eq!(config.effective_model(), "llama3.2");

    let config: Config =
        toml::from_str("provider = \"ollama-cloud\"").expect("the setting makes sense");
    assert_eq!(config.effective_model(), "gpt-oss:120b");
}
