//! MCP: talking to Model Context Protocol servers.
//!
//! An MCP server is another program that offers tools. It runs either as a
//! child process speaking JSON-RPC over its standard streams, or as an HTTP
//! service speaking JSON-RPC over POST. Either way the shape is the same:
//! initialize, list the tools, call them.
//!
//! Each server's tools are exposed under the name `<server>__<tool>`, so
//! two servers offering a `search` tool do not collide.

pub mod http;
pub mod server;
pub mod stdio;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::config::McpServerConfig;
use crate::tool::{Result as ToolResult, Tool, ToolError, ToolOutput};

/// The version of the protocol lia speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// A tool an MCP server offers.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// What the server says about the tool: whether it only reads, whether
    /// it destroys what was there, and the like.
    pub annotations: Option<Value>,
}

/// How an MCP server is reached.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a request and wait for its answer.
    async fn request(&self, method: &str, params: Value) -> Result<Value>;

    /// Send a notification, which has no answer.
    async fn notify(&self, method: &str, params: Value) -> Result<()>;
}

/// A connected MCP server.
pub struct McpClient {
    /// The name of the server, as configured.
    pub name: String,
    transport: Arc<dyn Transport>,
    /// The instructions the server sent with its handshake.
    pub instructions: Option<String>,
}

impl McpClient {
    /// Connect to a server and shake hands.
    pub async fn connect(name: &str, config: &McpServerConfig) -> Result<Self> {
        let transport: Arc<dyn Transport> = match config.transport {
            crate::config::McpTransport::Stdio => {
                let stdio = stdio::StdioTransport::spawn(config)
                    .await
                    .with_context(|| format!("could not start the MCP server '{name}'"))?;
                Arc::new(stdio)
            }
            crate::config::McpTransport::StreamableHttp | crate::config::McpTransport::Sse => {
                let http = http::HttpTransport::new(config)
                    .await
                    .with_context(|| format!("could not reach the MCP server '{name}'"))?;
                Arc::new(http)
            }
        };

        let mut client = McpClient {
            name: name.to_string(),
            transport,
            instructions: None,
        };

        client.initialize().await?;
        Ok(client)
    }

    /// The handshake.
    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "clientInfo": {
                        "name": "lia",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await?;

        self.instructions = result
            .get("instructions")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty());

        // The server expects to be told the handshake is done.
        let _ = self.notify("notifications/initialized", json!({})).await;

        Ok(())
    }

    /// Send a request and wait for the answer.
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.transport.request(method, params).await
    }

    /// Send a notification, which has no answer.
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.transport.notify(method, params).await
    }

    /// The tools the server offers.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(tools
            .into_iter()
            .filter_map(|tool| {
                let name = tool.get("name")?.as_str()?.to_string();
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let input_schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" }));
                let annotations = tool.get("annotations").cloned();
                Some(McpToolInfo {
                    name,
                    description,
                    input_schema,
                    annotations,
                })
            })
            .collect())
    }

    /// Call a tool on the server.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolOutput> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;

        // A server may report a failure in the result rather than as an
        // error response.
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = result_text(&result);
            return Ok(ToolOutput::error(text));
        }

        Ok(ToolOutput::ok(result_text(&result)))
    }
}

/// Flatten an MCP tool result into text.
///
/// The result carries a list of content blocks, which may be text, images or
/// anything else the server invents. Text is joined; other kinds are
/// described.
fn result_text(result: &Value) -> String {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut out = String::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            _ => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&block.to_string());
            }
        }
    }

    if out.is_empty() {
        out = result.to_string();
    }
    out
}

/// A tool belonging to an MCP server, exposed to the model.
pub struct McpTool {
    /// The name the model sees: `<server>__<tool>`.
    name: String,
    /// The name the tool has on its own server.
    remote_name: String,
    description: String,
    input_schema: Value,
    /// Whether the server says the tool only reads. `None` when the server
    /// said nothing.
    read_only: Option<bool>,
    client: Arc<Mutex<McpClient>>,
}

impl McpTool {
    pub fn new(server: &str, info: McpToolInfo, client: Arc<Mutex<McpClient>>) -> Self {
        let read_only = info
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("readOnlyHint"))
            .and_then(Value::as_bool);
        McpTool {
            name: format!("{server}__{}", info.name),
            remote_name: info.name,
            description: info.description,
            input_schema: info.input_schema,
            read_only,
            client,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.input_schema.clone()
    }

    /// Whether the model should ask the user before this tool runs.
    ///
    /// A server that says its tool changes something is believed: the tool
    /// asks first, the way lia's own writing tools do. A server that says
    /// nothing is left as it was — trusted, and never in the way — so
    /// third-party servers behave as they did before.
    fn requires_approval(&self) -> bool {
        self.read_only == Some(false)
    }

    async fn run(&self, args: Value) -> ToolResult<ToolOutput> {
        let client = self.client.lock().await;
        client
            .call_tool(&self.remote_name, args)
            .await
            .map_err(|e| ToolError::Failed(self.name.clone(), e.to_string()))
    }
}

/// The MCP servers lia knows about, and the tools they offer.
pub struct McpRegistry {
    clients: Vec<Arc<Mutex<McpClient>>>,
}

impl McpRegistry {
    /// Connect to every enabled server and collect their tools.
    ///
    /// A server that cannot be reached is reported and skipped: one broken
    /// server should not stop the assistant from working.
    pub async fn connect(servers: &HashMap<String, McpServerConfig>) -> Self {
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();

        let mut clients = Vec::new();
        for name in names {
            let config = &servers[name];
            if !config.enabled {
                continue;
            }
            match McpClient::connect(name, config).await {
                Ok(client) => {
                    tracing::info!("connected to the MCP server '{name}'");
                    clients.push(Arc::new(Mutex::new(client)));
                }
                Err(e) => {
                    tracing::warn!("the MCP server '{name}' is unavailable: {e:#}");
                }
            }
        }

        McpRegistry { clients }
    }

    /// Every tool the connected servers offer, named `<server>__<tool>`.
    pub async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        for client in &self.clients {
            let guard = client.lock().await;
            let server = guard.name.clone();
            match guard.list_tools().await {
                Ok(infos) => {
                    for info in infos {
                        tools.push(Arc::new(McpTool::new(&server, info, client.clone())));
                    }
                }
                Err(e) => {
                    tracing::warn!("could not list the tools of '{server}': {e:#}");
                }
            }
        }

        tools
    }

    /// The instructions the servers sent with their handshake.
    pub async fn instructions(&self) -> Vec<String> {
        let mut out = Vec::new();
        for client in &self.clients {
            let guard = client.lock().await;
            if let Some(text) = &guard.instructions {
                out.push(format!("{}: {text}", guard.name));
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}
