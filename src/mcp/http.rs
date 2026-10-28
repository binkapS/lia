//! Talking to an MCP server over HTTP.
//!
//! Two shapes are supported:
//!
//! - **streamable HTTP** — a POST carries a JSON-RPC message and the answer
//!   comes back in the same response, either as JSON or as an event stream.
//! - **SSE** — the older shape, where a long-lived GET stream carries the
//!   answers and a POST carries the questions. The server names the address
//!   to post to in its first event.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot, watch};

use super::Transport;
use super::stdio::{Pending, result_of};
use crate::config::McpServerConfig;

/// How the server carries its answers.
enum Mode {
    /// The answer comes back in the response to the POST.
    Streamable,
    /// The answer arrives on a long-lived event stream.
    Sse {
        endpoint: watch::Receiver<Option<String>>,
    },
}

/// An MCP server reached over HTTP.
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    /// The session the server handed out, if it hands one out.
    session_id: Mutex<Option<String>>,
    pending: Pending,
    next_id: AtomicI64,
    timeout: Duration,
    mode: Mode,
}

impl HttpTransport {
    /// Reach the server.
    ///
    /// For an SSE server this opens the event stream and waits for the
    /// server to say where to post, so a server that is not really there is
    /// reported here rather than at the first tool call.
    pub async fn new(config: &McpServerConfig) -> Result<Self> {
        let url = config.address()?.to_string();
        let timeout = config.timeout();

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("could not build an HTTP client")?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let next_id = AtomicI64::new(1);

        let mode = match config.transport {
            crate::config::McpTransport::Sse => {
                let (endpoint_tx, endpoint_rx) = watch::channel(None);
                let response = get_event_stream(&client, &url, &config.headers)
                    .await
                    .with_context(|| format!("could not open the event stream at {url}"))?;

                let task = tokio::spawn(read_events(
                    response,
                    pending.clone(),
                    endpoint_tx,
                    url.clone(),
                ));

                let endpoint = wait_for_endpoint(endpoint_rx.clone(), task, timeout).await?;
                Mode::Sse {
                    endpoint: watch::Receiver::clone(&endpoint),
                }
            }
            _ => Mode::Streamable,
        };

        Ok(HttpTransport {
            client,
            url,
            headers: config.headers.clone(),
            session_id: Mutex::new(None),
            pending,
            next_id,
            timeout,
            mode,
        })
    }

    /// Post a JSON-RPC message.
    async fn post(&self, url: &str, message: &Value) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(message);

        for (name, value) in &self.headers {
            request = request.header(name, value);
        }

        if let Some(session) = self.session_id.lock().await.as_ref() {
            request = request.header("mcp-session-id", session);
        }

        request
            .send()
            .await
            .with_context(|| format!("could not reach the MCP server at {url}"))
    }

    /// Read the answer out of a response.
    async fn read_response(&self, response: reqwest::Response, id: i64) -> Result<Value> {
        let status = response.status();

        // A server may hand out a session to carry on with.
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().await = Some(session.to_string());
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body = response.text().await?;

        if !status.is_success() {
            return Err(anyhow!("the MCP server answered {status}: {}", body.trim()));
        }

        if content_type.contains("text/event-stream") {
            let messages = sse_messages(&body);
            return pick(messages, id);
        }

        serde_json::from_str(&body)
            .map_err(|e| anyhow!("the MCP server sent something that is not JSON: {e}"))
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        match &self.mode {
            Mode::Streamable => {
                let response = self.post(&self.url, &message).await?;
                let answer = self.read_response(response, id).await?;
                result_of(&answer, method)
            }
            Mode::Sse { endpoint } => {
                let address = endpoint
                    .borrow()
                    .clone()
                    .ok_or_else(|| anyhow!("the MCP server never said where to post"))?;

                // Register before posting: the answer may be waiting on the
                // event stream already.
                let (tx, rx) = oneshot::channel();
                self.pending.lock().await.insert(id, tx);

                let response = self.post(&address, &message).await;
                if let Err(e) = response {
                    self.pending.lock().await.remove(&id);
                    return Err(e);
                }
                let response = response.unwrap();
                if !response.status().is_success() {
                    self.pending.lock().await.remove(&id);
                    return Err(anyhow!("the MCP server answered {}", response.status()));
                }

                let answer = tokio::time::timeout(self.timeout, rx)
                    .await
                    .map_err(|_| anyhow!("the MCP server did not answer '{method}' in time"))?
                    .map_err(|_| anyhow!("the MCP server stopped before answering '{method}'"))?;

                result_of(&answer, method)
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let url = match &self.mode {
            Mode::Streamable => self.url.clone(),
            Mode::Sse { endpoint } => endpoint
                .borrow()
                .clone()
                .ok_or_else(|| anyhow!("the MCP server never said where to post"))?,
        };

        let response = self.post(&url, &message).await?;
        if !response.status().is_success() {
            return Err(anyhow!("the MCP server answered {}", response.status()));
        }
        Ok(())
    }
}

/// Open the event stream of an SSE server.
async fn get_event_stream(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<reqwest::Response> {
    let mut request = client
        .get(url)
        .header("accept", "text/event-stream")
        .header("cache-control", "no-store");

    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(anyhow!("the server answered {}", response.status()));
    }
    Ok(response)
}

/// Read the event stream until it ends.
async fn read_events(
    response: reqwest::Response,
    pending: Pending,
    endpoint: watch::Sender<Option<String>>,
    base: String,
) {
    let mut stream = response.bytes_stream().eventsource();

    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(event) => event,
            Err(e) => {
                tracing::warn!("the MCP server's event stream broke: {e}");
                break;
            }
        };

        if event.event == "endpoint" {
            let address = resolve(&base, event.data.trim());
            let _ = endpoint.send(Some(address));
            continue;
        }

        let data = event.data.trim();
        if data.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_i64) else {
            continue;
        };
        let sender = pending.lock().await.remove(&id);
        if let Some(tx) = sender {
            let _ = tx.send(value);
        }
    }

    pending.lock().await.clear();
}

/// Wait for the server to name the address to post to.
async fn wait_for_endpoint(
    mut endpoint: watch::Receiver<Option<String>>,
    task: tokio::task::JoinHandle<()>,
    timeout: Duration,
) -> Result<watch::Receiver<Option<String>>> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut task = task;

    loop {
        if endpoint.borrow().is_some() {
            return Ok(endpoint);
        }
        tokio::select! {
            _ = endpoint.changed() => {}
            _ = tokio::time::sleep_until(deadline) => {
                task.abort();
                return Err(anyhow!("the MCP server never said where to post"));
            }
            result = &mut task => {
                return match result {
                    Ok(()) => Err(anyhow!("the MCP server closed the event stream")),
                    Err(e) => Err(anyhow!("the event stream failed: {e}")),
                };
            }
        }
    }
}

/// Work out the full address of the endpoint the server named.
fn resolve(base: &str, target: &str) -> String {
    if target.is_empty() {
        return base.to_string();
    }
    if target.starts_with("http://") || target.starts_with("https://") {
        return target.to_string();
    }
    match reqwest::Url::parse(base).and_then(|url| url.join(target)) {
        Ok(url) => url.to_string(),
        Err(_) => target.to_string(),
    }
}

/// Pull the JSON-RPC messages out of an event-stream body.
fn sse_messages(body: &str) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut data = String::new();

    for line in body.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if let Some(value) = parse_frame(&data) {
                messages.push(value);
            }
            data.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    if let Some(value) = parse_frame(&data) {
        messages.push(value);
    }

    messages
}

/// Parse one event's worth of data.
fn parse_frame(data: &str) -> Option<Value> {
    let data = data.trim();
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(data).ok()
}

/// Choose the message that answers the request.
fn pick(messages: Vec<Value>, id: i64) -> Result<Value> {
    for message in &messages {
        if message.get("id").and_then(Value::as_i64) == Some(id) {
            return Ok(message.clone());
        }
    }
    messages
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("the MCP server sent no answer"))
}
