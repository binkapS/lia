//! Shell command execution.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use super::sandbox::Sandbox;
use super::{Result, Tool, ToolError, ToolOutput};

/// Run a shell command in the workspace.
pub struct ShellTool {
    cwd: String,
    working_dir: std::sync::Mutex<std::path::PathBuf>,
    timeout: Duration,
    /// Command prefixes that are refused outright, whatever the approval policy.
    blocked: Vec<String>,
    /// Command prefixes that are permitted; empty means all (subject to blocked).
    allowed: Vec<String>,
    /// The sandbox the command is run in, if there is one.
    sandbox: Option<Sandbox>,
    /// Whether the commands are confined, which the model is told about.
    confined: bool,
}

impl ShellTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        let cwd = cwd.into();
        ShellTool {
            cwd: cwd.clone(),
            working_dir: std::sync::Mutex::new(std::path::PathBuf::from(&cwd)),
            timeout: Duration::from_secs(120),
            blocked: vec![
                "rm -rf /".into(),
                "mkfs".into(),
                ":(){".into(), // fork bomb
                "dd if=".into(),
                "shutdown".into(),
                "reboot".into(),
            ],
            allowed: Vec::new(),
            sandbox: None,
            confined: false,
        }
    }

    /// Run the commands in a sandbox.
    pub fn with_sandbox(mut self, sandbox: Sandbox) -> ShellTool {
        self.confined = sandbox.is_enabled();
        self.sandbox = Some(sandbox);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Permit only commands whose lowercased form starts with one of these.
    pub fn with_allowed(mut self, allowed: Vec<String>) -> Self {
        self.allowed = allowed;
        self
    }

    /// Block an additional command pattern.
    pub fn block(mut self, pattern: impl Into<String>) -> Self {
        self.blocked.push(pattern.into());
        self
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "run_shell"
    }

    fn description(&self) -> &str {
        if self.confined {
            // The model is told, so it does not try to write outside the
            // workspace and wonder why it cannot.
            "Run a shell command in the workspace and return its output. \
             Long-running commands are killed after the timeout. The command \
             runs in a sandbox: the workspace is the only place it may write, \
             and the rest of the disk can only be read."
        } else {
            "Run a shell command in the workspace and return its output. Long-running commands are killed after the timeout."
        }
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to run, e.g. 'cargo test'."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Override the default timeout in seconds."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command."
                },
                "output_format": {
                    "type": "string",
                    "description": "Output format: 'text' (default) or 'json'."
                }
            },
            "required": ["command"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let command =
            args.get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'command'".into(),
                })?;

        let lower = command.to_lowercase();
        for pattern in &self.blocked {
            if lower.contains(&pattern.to_lowercase()) {
                return Err(ToolError::Failed(
                    self.name().into(),
                    format!("command blocked by policy: matches '{pattern}'"),
                ));
            }
        }

        if !self.allowed.is_empty()
            && !self
                .allowed
                .iter()
                .any(|a| lower.starts_with(&a.to_lowercase()))
        {
            return Err(ToolError::Failed(
                self.name().into(),
                "command not allowed by policy".into(),
            ));
        }

        let timeout = args
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or(self.timeout);

        let param_cwd = args.get("cwd").and_then(Value::as_str);
        let cwd = if let Some(c) = param_cwd {
            let p = PathBuf::from(c);
            if let Ok(mut guard) = self.working_dir.lock() {
                *guard = p.clone();
            }
            p
        } else {
            self.working_dir
                .lock()
                .map(|g| g.clone())
                .unwrap_or_else(|_| PathBuf::from(&self.cwd))
        };
        let output_format = args
            .get("output_format")
            .and_then(Value::as_str)
            .unwrap_or("text");

        // The command may be run in a sandbox, where the rest of the disk
        // can only be read. Where there is no sandbox it runs as it did.
        let wrapped = match &self.sandbox {
            Some(sandbox) => sandbox.wrap(command).await,
            None => None,
        };

        // Run through the shell so pipes and redirects work as the model expects.
        let mut process = match wrapped {
            Some(args) => {
                let (program, rest) = args.split_first().expect("there is a program to run");
                let mut process = Command::new(program);
                process.args(rest);
                process
            }
            None => {
                let mut process = Command::new("bash");
                process.arg("-lc").arg(command);
                process
            }
        };

        let child = process
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(ToolError::Failed(self.name().into(), e.to_string())),
            Err(_) => {
                return Err(ToolError::Timeout(self.name().into()));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut text = String::new();
        if !stdout.trim().is_empty() {
            text.push_str(stdout.trim_end());
        }
        if !stderr.trim().is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str("[stderr]\n");
            text.push_str(stderr.trim_end());
        }
        if text.is_empty() {
            text = "(no output)".to_string();
        }

        let code = output.status.code();
        let is_error = !output.status.success();

        let metadata = json!({
            "exit_code": code,
            "command": command,
            "cwd": cwd.display().to_string(),
            "timeout_seconds": timeout.as_secs(),
        });

        let content = if output_format == "json" {
            json!({
                "stdout": stdout.trim_end(),
                "stderr": stderr.trim_end(),
                "exit_code": code,
            })
            .to_string()
        } else {
            text
        };

        Ok(ToolOutput {
            content,
            is_error,
            metadata: Some(metadata),
        })
    }
}
