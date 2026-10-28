//! Tests for skill discovery, the prompt catalogue, and the skill tool.

use std::path::PathBuf;

use lia::skill::{SkillSet, SkillSource, SkillTool, parse_frontmatter};
use lia::tool::Tool;
use serde_json::json;

/// Write a skill directory with a `SKILL.md`.
fn write_skill(root: &std::path::Path, name: &str, frontmatter: &str, body: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let text = if frontmatter.is_empty() {
        body.to_string()
    } else {
        format!("---\n{frontmatter}---\n{body}")
    };
    std::fs::write(dir.join("SKILL.md"), text).unwrap();
}

#[test]
fn the_frontmatter_is_split_from_the_body() {
    let (fields, body) =
        parse_frontmatter("---\nname: deploy\ndescription: Ship the thing\n---\n\nDo the steps.\n");

    assert_eq!(fields[0], ("name".to_string(), "deploy".to_string()));
    assert_eq!(
        fields[1],
        ("description".to_string(), "Ship the thing".to_string())
    );
    assert_eq!(body, "Do the steps.\n");
}

#[test]
fn a_quoted_frontmatter_value_is_unquoted() {
    let (fields, _) = parse_frontmatter("---\nname: \"my skill\"\n---\nbody\n");
    assert_eq!(fields[0].1, "my skill");
}

#[test]
fn a_file_without_frontmatter_is_all_body() {
    let (fields, body) = parse_frontmatter("Just some text.\n");
    assert!(fields.is_empty());
    assert_eq!(body, "Just some text.\n");
}

#[test]
fn skills_are_found_in_a_root() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "deploy",
        "name: deploy\ndescription: Ship the thing\n",
        "Step one.\n",
    );
    write_skill(
        dir.path(),
        "review",
        "name: review\ndescription: Check the work\n",
        "Step two.\n",
    );

    let skills = SkillSet::discover(&[(SkillSource::Workspace, dir.path().to_path_buf())]);

    assert_eq!(skills.len(), 2);
    let deploy = skills.get("deploy").expect("the deploy skill");
    assert_eq!(deploy.description, "Ship the thing");
    assert_eq!(deploy.body, "Step one.\n");
    assert_eq!(deploy.source, SkillSource::Workspace);
    assert!(skills.get("DEPLOY").is_some(), "the lookup ignores case");
}

#[test]
fn a_skill_without_a_name_uses_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(dir.path(), "fallback", "", "Just do the thing.\n");

    let skills = SkillSet::discover(&[(SkillSource::Personal, dir.path().to_path_buf())]);
    let skill = skills.get("fallback").expect("the skill");
    assert_eq!(skill.name, "fallback");
    // With no description, the first line of the body stands in.
    assert_eq!(skill.description, "Just do the thing.");
}

#[test]
fn an_earlier_root_wins_over_a_later_one() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_skill(
        first.path(),
        "shared",
        "name: shared\ndescription: The project's version\n",
        "a\n",
    );
    write_skill(
        second.path(),
        "shared",
        "name: shared\ndescription: The personal version\n",
        "b\n",
    );

    let skills = SkillSet::discover(&[
        (SkillSource::Workspace, first.path().to_path_buf()),
        (SkillSource::Personal, second.path().to_path_buf()),
    ]);

    assert_eq!(skills.len(), 1, "only one skill of a given name");
    let skill = skills.get("shared").unwrap();
    assert_eq!(skill.description, "The project's version");
}

#[test]
fn the_catalogue_is_xml_and_names_every_skill() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "deploy",
        "name: deploy\ndescription: Ship the thing\n",
        "a\n",
    );

    let skills = SkillSet::discover(&[(SkillSource::Workspace, dir.path().to_path_buf())]);
    let catalogue = skills.catalog_prompt();

    assert!(catalogue.starts_with("<available_skills>"));
    assert!(catalogue.ends_with("</available_skills>"));
    assert!(catalogue.contains("<name>deploy</name>"));
    assert!(catalogue.contains("<description>Ship the thing</description>"));
    assert!(catalogue.contains("<location>"));
}

#[test]
fn an_empty_catalogue_is_empty() {
    let skills = SkillSet::default();
    assert!(skills.catalog_prompt().is_empty());
    assert!(skills.is_empty());
}

#[tokio::test]
async fn the_skill_tool_lists_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "deploy",
        "name: deploy\ndescription: Ship the thing\n",
        "Step one.\nStep two.\n",
    );

    let skills = std::sync::Arc::new(SkillSet::discover(&[(
        SkillSource::Workspace,
        dir.path().to_path_buf(),
    )]));
    let tool = SkillTool::new(skills);

    let listing = tool.run(json!({})).await.unwrap();
    assert!(listing.content.contains("deploy"));
    assert!(listing.content.contains("Ship the thing"));

    let loaded = tool.run(json!({ "name": "deploy" })).await.unwrap();
    assert!(loaded.content.contains("Step one."));
    assert!(loaded.content.contains("Step two."));
}

#[tokio::test]
async fn asking_for_a_skill_that_does_not_exist_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "deploy",
        "name: deploy\ndescription: Ship the thing\n",
        "a\n",
    );

    let skills = std::sync::Arc::new(SkillSet::discover(&[(
        SkillSource::Workspace,
        dir.path().to_path_buf(),
    )]));
    let tool = SkillTool::new(skills);

    let error = tool.run(json!({ "name": "nope" })).await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("no skill named 'nope'"), "got: {message}");
    // The message says what is available, so the model can correct itself.
    assert!(message.contains("deploy"), "got: {message}");
}

#[test]
fn a_description_is_shortened_for_the_catalogue() {
    let dir = tempfile::tempdir().unwrap();
    let long = "x".repeat(500);
    write_skill(
        dir.path(),
        "verbose",
        &format!("name: verbose\ndescription: {long}\n"),
        "a\n",
    );

    let skills = SkillSet::discover(&[(SkillSource::Workspace, dir.path().to_path_buf())]);
    let catalogue = skills.catalog_prompt();

    assert!(catalogue.len() < 500, "the catalogue should be shortened");
    assert!(catalogue.contains('…'));
}

#[test]
fn a_skill_larger_than_the_limit_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(lia::skill::MAX_SKILL_BYTES as usize + 10);
    write_skill(
        dir.path(),
        "huge",
        "name: huge\ndescription: Too big\n",
        &big,
    );

    let skills = SkillSet::discover(&[(SkillSource::Workspace, dir.path().to_path_buf())]);
    assert!(skills.is_empty(), "an oversized skill should be skipped");
}

#[test]
fn a_missing_root_is_not_an_error() {
    let skills = SkillSet::discover(&[(
        SkillSource::Workspace,
        PathBuf::from("/definitely/not/here"),
    )]);
    assert!(skills.is_empty());
}

#[test]
fn the_builtin_skills_are_always_available() {
    let mut skills = SkillSet::default();
    skills.add_builtins();

    for name in [
        "system-health",
        "system-diagnostics",
        "security-audit",
        "deployment-readiness",
    ] {
        let skill = skills.get(name).expect("the built-in skill is present");
        assert_eq!(skill.source, SkillSource::Builtin);
        assert!(!skill.description.trim().is_empty());
        assert!(!skill.body.trim().is_empty());
    }

    // Adding them twice does not double the set.
    let count = skills.len();
    skills.add_builtins();
    assert_eq!(skills.len(), count);
}

#[test]
fn a_project_skill_shadows_a_builtin_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "system-health",
        "name: system-health\ndescription: The project's own runbook\n",
        "Follow the project steps.\n",
    );

    let mut skills = SkillSet::discover(&[(SkillSource::Workspace, dir.path().to_path_buf())]);
    skills.add_builtins();

    let skill = skills.get("system-health").expect("the skill is present");
    assert_eq!(skill.source, SkillSource::Workspace);
    assert_eq!(skill.description, "The project's own runbook");
}
