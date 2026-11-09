//! Talking to an MCP server that runs as a child process.
//!
//! The server is started with its standard streams piped. Each JSON-RPC
//! message is one line of JSON: one line out, one line in. A reader task
//! watches the output and hands each answer to whoever is waiting for it.

use std::collections::HashMap;
use std::process::Stdio as ProcessStdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, oneshot};

use super::Transport;
use crate::config::McpServerConfig;

/// The answers still waiting to be collected.
pub type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

/// An MCP server running as a child process.
pub struct StdioTransport {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicI64,
    timeout: Duration,
}

impl StdioTransport {
    /// Start the server and begin reading its answers.
    pub async fn spawn(config: &McpServerConfig) -> Result<Self> {
        let (command, args) = config.command_line()?;
        let mut cmd = Command::new(&command);
        cmd.args(&args)
            .stdin(ProcessStdio::piped())
            .stdout(ProcessStdio::piped())
            .stderr(ProcessStdio::piped())
            .kill_on_drop(true);

        if let Some(cwd) = &config.cwd {
            cmd.current_dir(cwd);
        }

        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not run '{command}'"))?;

        let stdin = child.stdin.take().ok_or_else(|| {
            anyhow!("the MCP server '{command}' was started without an input stream")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow!("the MCP server '{command}' was started without an output stream")
        })?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(read_answers(stdout, pending.clone()));

        // The server's own logging goes to its error stream. It is worth
        // seeing when something goes wrong, so it is passed through.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(read_complaints(stderr));
        }

        Ok(StdioTransport {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicI64::new(1),
            timeout: config.timeout(),
        })
    }

    /// Write one message to the server.
    async fn write(&self, message: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // The server is a child process; it should not outlive us.
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.start_kill();
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        if let Err(e) = self.write(&message).await {
            self.pending.lock().await.remove(&id);
            return Err(e.context(format!("could not talk to the MCP server over '{method}'")));
        }

        let answer = tokio::time::timeout(self.timeout, rx)
            .await
            .map_err(|_| anyhow!("the MCP server did not answer '{method}' in time"))?
            .map_err(|_| anyhow!("the MCP server stopped before answering '{method}'"))?;

        result_of(&answer, method)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write(&message).await
    }
}

/// Pull the result out of a JSON-RPC response, turning an error response
/// into an error.
pub fn result_of(response: &Value, method: &str) -> Result<Value> {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the MCP server reported an error");
        return Err(anyhow!("the MCP server refused '{method}': {message}"));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

/// Read the server's answers until its output ends.
async fn read_answers(stdout: ChildStdout, pending: Pending) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    tracing::warn!("the MCP server wrote a line that is not JSON");
                    continue;
                };
                // Only a response has an id; a notification is not ours.
                let Some(id) = value.get("id").and_then(Value::as_i64) else {
                    continue;
                };
                let sender = pending.lock().await.remove(&id);
                if let Some(tx) = sender {
                    let _ = tx.send(value);
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("the MCP server stopped talking: {e}");
                break;
            }
        }
    }

    // Everything still waiting will never be answered.
    pending.lock().await.clear();
}

/// Pass the server's error output through to the log.
async fn read_complaints(stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!("mcp server: {line}");
    }
}
