//! Tests for the MCP client.
//!
//! The test binary doubles as a mock MCP server: when it is started with
//! `LIA_MCP_MOCK` set, it speaks JSON-RPC over its standard streams instead
//! of running the tests. That keeps the test free of any other program.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::pin::Pin;
use std::sync::Arc;

use lia::config::{McpServerConfig, McpTransport};
use lia::mcp::{McpClient, McpRegistry};
use serde_json::{Value, json};

/// The environment variable that turns the test binary into a server.
const MOCK: &str = "LIA_MCP_MOCK";

fn main() {
    if std::env::var(MOCK).is_ok() {
        serve();
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("could not start a runtime");
    type TestFn = Pin<Box<dyn std::future::Future<Output = ()>>>;
    let tests: Vec<(&str, TestFn)> = vec![
        ("the handshake is answered", Box::pin(handshake())),
        (
            "the tools of a server are listed",
            Box::pin(tools_are_listed()),
        ),
        (
            "a tool call reaches the server",
            Box::pin(calls_reach_the_server()),
        ),
        (
            "two servers do not collide",
            Box::pin(servers_do_not_collide()),
        ),
        (
            "a broken server is skipped",
            Box::pin(broken_server_is_skipped()),
        ),
        (
            "an error from the server is reported",
            Box::pin(errors_are_reported()),
        ),
        ("an http server is reached", Box::pin(http_is_reached())),
        (
            "a server that is not there is reported",
            Box::pin(missing_server_is_reported()),
        ),
    ];

    let mut failed = 0;
    for (name, test) in tests {
        print!("test {name} ... ");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(test);
        }));
        match result {
            Ok(()) => println!("ok"),
            Err(_) => {
                failed += 1;
                println!("FAILED");
            }
        }
    }

    if failed > 0 {
        eprintln!("\n{failed} test(s) failed");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------- the tests

fn stdio_config() -> McpServerConfig {
    let program = std::env::current_exe().expect("no path to the test binary");
    McpServerConfig {
        transport: McpTransport::Stdio,
        command: Some(program.to_string_lossy().to_string()),
        args: Vec::new(),
        timeout_secs: 10,
        env: HashMap::from([(MOCK.to_string(), "1".to_string())]),
        ..Default::default()
    }
}

async fn handshake() {
    let client = McpClient::connect("mock", &stdio_config())
        .await
        .expect("the handshake failed");

    assert_eq!(
        client.instructions.as_deref(),
        Some("the mock server"),
        "the instructions were not kept"
    );
}

async fn tools_are_listed() {
    let client = McpClient::connect("mock", &stdio_config())
        .await
        .expect("the handshake failed");

    let tools = client.list_tools().await.expect("could not list the tools");
    assert_eq!(tools.len(), 1, "got {tools:?}");
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].description, "says what it is told");
    assert_eq!(tools[0].input_schema["type"], json!("object"));
}

async fn calls_reach_the_server() {
    let client = McpClient::connect("mock", &stdio_config())
        .await
        .expect("the handshake failed");

    let output = client
        .call_tool("echo", json!({ "text": "hello" }))
        .await
        .expect("the call failed");

    assert!(
        output.content.contains("echo: hello"),
        "got: {}",
        output.content
    );
}

async fn servers_do_not_collide() {
    let mut servers = HashMap::new();
    servers.insert("one".to_string(), stdio_config());
    servers.insert("two".to_string(), stdio_config());

    let registry = McpRegistry::connect(&servers).await;
    let tools = registry.tools().await;

    let mut names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["one__echo".to_string(), "two__echo".to_string()]
    );
}

async fn broken_server_is_skipped() {
    let mut servers = HashMap::new();
    servers.insert("good".to_string(), stdio_config());
    servers.insert(
        "broken".to_string(),
        McpServerConfig {
            transport: McpTransport::Stdio,
            command: Some("/nonexistent/mcp-server".to_string()),
            timeout_secs: 5,
            ..Default::default()
        },
    );

    let registry = McpRegistry::connect(&servers).await;

    // The good server still works.
    let tools = registry.tools().await;
    assert_eq!(tools.len(), 1, "got {} tools", tools.len());
    assert_eq!(tools[0].name(), "good__echo");
}

async fn errors_are_reported() {
    let client = McpClient::connect("mock", &stdio_config())
        .await
        .expect("the handshake failed");

    // The mock refuses a tool it does not have.
    let error = client
        .call_tool("missing", json!({}))
        .await
        .expect_err("the error was not reported");
    assert!(error.to_string().contains("no such tool"), "got: {error}");
}

async fn http_is_reached() {
    let (address, handle) = start_http_server().await;

    let config = McpServerConfig {
        transport: McpTransport::StreamableHttp,
        url: Some(format!("http://{address}/mcp")),
        timeout_secs: 10,
        ..Default::default()
    };

    let client = McpClient::connect("web", &config)
        .await
        .expect("the handshake failed");

    let tools = client.list_tools().await.expect("could not list the tools");
    assert_eq!(tools.len(), 1, "got {tools:?}");
    assert_eq!(tools[0].name, "echo");

    let output = client
        .call_tool("echo", json!({ "text": "over http" }))
        .await
        .expect("the call failed");
    assert!(
        output.content.contains("echo: over http"),
        "got: {}",
        output.content
    );

    handle.abort();
}

async fn missing_server_is_reported() {
    // Bind and drop, so the port is almost certainly closed.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let config = McpServerConfig {
        transport: McpTransport::StreamableHttp,
        url: Some(format!("http://{address}/mcp")),
        timeout_secs: 5,
        ..Default::default()
    };

    let result = McpClient::connect("gone", &config).await;
    assert!(result.is_err(), "a missing server was not reported");
}

// ------------------------------------------------------------ the mock server

/// Speak JSON-RPC over the standard streams.
fn serve() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        // A notification has no id and is not answered.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let result = match method.as_str() {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mock", "version": "1" },
                "instructions": "the mock server",
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "echo",
                    "description": "says what it is told",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "text": { "type": "string" } },
                        "required": ["text"],
                    },
                }],
            }),
            "tools/call" => {
                let name = message
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if name != "echo" {
                    // A JSON-RPC error, as a real server would send it.
                    let _ = writeln!(
                        out,
                        "{}",
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32602, "message": "no such tool" },
                        })
                    );
                    let _ = out.flush();
                    continue;
                }
                let text = message
                    .pointer("/params/arguments/text")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json!({
                    "content": [{ "type": "text", "text": format!("echo: {text}") }]
                })
            }
            _ => json!({
                "error": { "code": -32601, "message": "no such method" }
            }),
        };

        let response = if result.get("error").is_some() {
            json!({ "jsonrpc": "2.0", "id": id, "error": result["error"] })
        } else {
            json!({ "jsonrpc": "2.0", "id": id, "result": result })
        };
        if writeln!(out, "{response}").is_err() {
            break;
        }
        let _ = out.flush();
    }
}

// -------------------------------------------------------- an http mock server

/// Start a server that speaks MCP over streamable HTTP.
async fn start_http_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use axum::{Json, Router, routing::post};

    async fn handle(body: String) -> Json<Value> {
        let Ok(message) = serde_json::from_str::<Value>(&body) else {
            return Json(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": "not JSON" },
            }));
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");

        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "web", "version": "1" },
            }),
            "tools/list" => json!({
                "tools": [{
                    "name": "echo",
                    "description": "says what it is told",
                    "inputSchema": { "type": "object" },
                }],
            }),
            "tools/call" => {
                let text = message
                    .pointer("/params/arguments/text")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json!({
                    "content": [{ "type": "text", "text": format!("echo: {text}") }]
                })
            }
            _ => json!({
                "error": { "code": -32601, "message": "no such method" }
            }),
        };

        Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    let app = Router::new().route("/mcp", post(handle));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (address, handle)
}

// Silence the unused warning when the mock server is not the one running.
#[allow(dead_code)]
fn _keep_arc_used(_: Arc<()>) {}
