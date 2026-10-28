//! Tests of the sandbox shell commands run in.
//!
//! A stand-in for bubblewrap stands in for bubblewrap: it writes down how
//! it was called and then does what it was told, so the path a command
//! takes can be checked without a kernel that says yes.

use std::path::{Path, PathBuf};

use lia::tool::Tool;
use lia::tool::sandbox::{Sandbox, SandboxConfig};
use lia::tool::shell::ShellTool;
use serde_json::json;

/// A directory of its own for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lia-sandbox-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("the directory is made");
    dir
}

/// A stand-in for bubblewrap.
///
/// It writes down how it was called, and then runs what it was told, so
/// the command still happens.
fn fake_bwrap(dir: &Path) -> (PathBuf, PathBuf) {
    let program = dir.join("fake-bwrap");
    let record = dir.join("args.txt");

    // The stand-in writes down how it was called. It does not understand
    // the options, so it runs the last three arguments — the shell and the
    // command — and treats the probe's "true" as the probe it is.
    let script = r#"#!/bin/sh
printf '%s\n' "$@" > RECORD
n=$#
last=
for a in "$@"; do last=$a; done
if [ "$last" = true ]; then exit 0; fi
eval "set -- \"\${$((n-2))}\" \"\${$((n-1))}\" \"\${$n}}\""
exec "$@"
"#;
    std::fs::write(
        &program,
        script.replace("RECORD", &record.display().to_string()),
    )
    .expect("the stand-in is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&program)
            .expect("the stand-in is there")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).expect("the stand-in may be run");
    }
    (program, record)
}

/// The tool, with a sandbox that works.
fn tool_with_sandbox(dir: &Path) -> (ShellTool, PathBuf) {
    let (program, record) = fake_bwrap(dir);
    let sandbox = Sandbox::new(
        SandboxConfig {
            enabled: true,
            network: true,
            hide_home: false,
        },
        dir,
    )
    .with_program(program);
    (
        ShellTool::new(dir.to_string_lossy().to_string()).with_sandbox(sandbox),
        record,
    )
}

#[tokio::test]
async fn the_model_is_told_when_commands_are_confined() {
    let dir = scratch("described");
    let (tool, _record) = tool_with_sandbox(&dir);
    assert!(
        tool.description().contains("sandbox"),
        "the model is not told: {}",
        tool.description()
    );

    let plain = ShellTool::new(dir.to_string_lossy().to_string());
    assert!(
        !plain.description().contains("sandbox"),
        "the model is told about a sandbox that is not there"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_command_runs_inside_the_sandbox() {
    let dir = scratch("inside");
    let (tool, record) = tool_with_sandbox(&dir);

    let output = tool
        .run(json!({ "command": "echo hello from inside" }))
        .await
        .expect("the command runs");

    assert!(
        output.content.contains("hello from inside"),
        "the command did not run: {}",
        output.content
    );

    let args = std::fs::read_to_string(&record).expect("the call was written down");
    assert!(args.contains("--ro-bind"), "{args}");
    assert!(args.contains("--bind"), "{args}");
    assert!(
        args.contains("echo hello from inside"),
        "the command was not passed on: {args}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_sandbox_that_does_not_work_leaves_the_command_alone() {
    let dir = scratch("nowork");
    let sandbox = Sandbox::new(
        SandboxConfig {
            enabled: true,
            network: true,
            hide_home: false,
        },
        dir.clone(),
    )
    // A program that is not there stands in for a bubblewrap that cannot
    // be run.
    .with_program("/nonexistent/bwrap");
    let tool = ShellTool::new(dir.to_string_lossy().to_string()).with_sandbox(sandbox);

    let output = tool
        .run(json!({ "command": "echo hello anyway" }))
        .await
        .expect("the command runs");

    assert!(
        output.content.contains("hello anyway"),
        "the command did not run: {}",
        output.content
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_sandbox_that_is_switched_off_leaves_the_command_alone() {
    let dir = scratch("off");
    let sandbox = Sandbox::new(
        SandboxConfig {
            enabled: false,
            network: true,
            hide_home: false,
        },
        dir.clone(),
    );
    let tool = ShellTool::new(dir.to_string_lossy().to_string()).with_sandbox(sandbox);

    let output = tool
        .run(json!({ "command": "echo hello again" }))
        .await
        .expect("the command runs");

    assert!(
        output.content.contains("hello again"),
        "the command did not run: {}",
        output.content
    );

    let _ = std::fs::remove_dir_all(dir);
}
