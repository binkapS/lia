//! Anthropic Messages API backend.
//!
//! Translates the neutral types into the Anthropic wire format and parses the
//! streaming SSE events back out.

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::types::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ModelInfo, StopReason,
    StreamEvent, ToolSpec, Usage,
};
use super::{EventStream, Provider, ProviderError, Result};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// The Anthropic Messages API provider.
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: usize,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        AnthropicProvider {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model,
            max_tokens: 8192,
        }
    }

    /// Point the provider at a different endpoint (proxies, gateways).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .map_err(|_| ProviderError::Auth("invalid api key characters".into()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        Ok(headers)
    }

    /// Build the request body for the Anthropic Messages API.
    fn body(&self, request: &CompletionRequest, stream: bool) -> Value {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_output_tokens.unwrap_or(self.max_tokens),
            "messages": request.messages.iter().map(encode_message).collect::<Vec<_>>(),
        });

        if let Some(system) = &request.system {
            body["system"] = json!(system);
        }
        if !request.tools.is_empty() {
            body["tools"] = json!(request.tools.iter().map(encode_tool).collect::<Vec<_>>());
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        if stream {
            body["stream"] = json!(true);
        }
        if let Some(extra) = &request.extra
            && let (Some(obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object())
        {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
        body
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .headers(self.headers()?)
            .json(body)
            .send()
            .await?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        Err(status_error(status.as_u16(), response).await)
    }
}

/// Build a provider error from a non-success response, mapping well-known
/// status codes onto specific error variants.
pub(crate) async fn status_error(status: u16, response: reqwest::Response) -> ProviderError {
    let body = response.text().await.unwrap_or_default();

    match status {
        401 | 403 => ProviderError::Auth(body),
        404 => ProviderError::ModelNotFound(body),
        429 => ProviderError::RateLimited(body),
        413 => ProviderError::ContextOverflow(body),
        400 => {
            // Anthropic signals an oversized prompt with a specific error type.
            if body.contains("prompt is too long") || body.contains("context") {
                ProviderError::ContextOverflow(body)
            } else {
                ProviderError::InvalidRequest(body)
            }
        }
        _ => ProviderError::Http { status, body },
    }
}

/// Encode one neutral message into Anthropic's block format.
pub fn encode_message(message: &Message) -> Value {
    let mut content = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                content.push(json!({ "type": "text", "text": text }));
            }
            ContentBlock::ToolUse { id, name, input } => {
                content.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content: result,
                is_error,
            } => {
                content.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": result,
                    "is_error": is_error,
                }));
            }
            ContentBlock::Image { media_type, data } => {
                content.push(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data },
                }));
            }
            ContentBlock::Thinking { .. } => {
                // Anthropic's own thinking blocks carry a signature that
                // only the model that wrote them can produce, so ours are
                // left out rather than sent and refused.
            }
        }
    }

    // Anthropic requires the "tool" role to carry tool results.
    let role = match message.role {
        super::types::Role::Assistant => "assistant",
        super::types::Role::User => "user",
        super::types::Role::Tool => "user",
        super::types::Role::System => "user",
    };

    json!({ "role": role, "content": content })
}

fn encode_tool(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}

/// Map Anthropic's stop_reason onto the neutral one.
fn map_stop_reason(stop: Option<&str>) -> Option<StopReason> {
    Some(match stop? {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    })
}

fn parse_content_blocks(raw: &Value) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let Some(array) = raw.as_array() else {
        return blocks;
    };
    for item in array {
        let block = match item.get("type").and_then(Value::as_str) {
            Some("text") => ContentBlock::Text {
                text: item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            Some("tool_use") => ContentBlock::ToolUse {
                id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: item.get("input").cloned().unwrap_or(json!({})),
            },
            _ => continue,
        };
        blocks.push(block);
    }
    blocks
}

fn parse_usage(raw: &Value) -> Option<Usage> {
    let input = raw.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output = raw
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if input == 0 && output == 0 {
        None
    } else {
        Some(Usage {
            input_tokens: input,
            output_tokens: output,
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let body = self.body(request, false);
        let response = self.post(&body).await?;
        let raw: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Decode(e.to_string()))?;

        Ok(CompletionResponse {
            content: parse_content_blocks(raw.get("content").unwrap_or(&Value::Null)),
            model: raw
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&self.model)
                .to_string(),
            stop_reason: map_stop_reason(raw.get("stop_reason").and_then(Value::as_str)),
            usage: raw.get("usage").and_then(parse_usage),
        })
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        let body = self.body(request, true);
        let response = self.post(&body).await?;

        let mut buffer = String::new();
        let mut pending_tool: Option<(String, String)> = None;

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();
            let mut saw_done = false;

            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(ProviderError::Network(e));
                        break;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // SSE frames are separated by a blank line.
                while let Some(pos) = buffer.find("\n\n") {
                    let frame = buffer[..pos].to_string();
                    buffer.drain(..pos + 2);

                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data.is_empty() { continue; }
                        if data == "[DONE]" { continue; }

                        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
                        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");

                        match kind {
                            "message_start" => {
                                if let Some(usage) = event.get("message").and_then(|m| m.get("usage")).and_then(parse_usage) {
                                    yield Ok(StreamEvent::Ping);
                                    let _ = usage;
                                }
                            }
                            "content_block_start" => {
                                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                                    let id = block.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                                    let name = block.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                                    pending_tool = Some((id.clone(), name.clone()));
                                    yield Ok(StreamEvent::ToolCallStart { id, name });
                                }
                            }
                            "content_block_delta" => {
                                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                                match delta.get("type").and_then(Value::as_str) {
                                    Some("text_delta") => {
                                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                            yield Ok(StreamEvent::text(text));
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str)
                                            && let Some((id, _)) = &pending_tool {
                                                yield Ok(StreamEvent::ToolCallDelta {
                                                    id: id.clone(),
                                                    arguments: partial.to_string(),
                                                });
                                            }
                                    }
                                    _ => {}
                                }
                            }
                            "content_block_stop" => {
                                if let Some((id, _)) = pending_tool.take() {
                                    yield Ok(StreamEvent::ToolCallEnd { id });
                                }
                            }
                            "message_delta" => {
                                let stop = event
                                    .get("delta")
                                    .and_then(|d| d.get("stop_reason"))
                                    .and_then(Value::as_str);
                                let usage = event.get("usage").and_then(parse_usage);
                                if stop.is_some() || usage.is_some() {
                                    yield Ok(StreamEvent::Done { stop_reason: map_stop_reason(stop), usage });
                                }
                            }
                            "message_stop" => {
                                if !saw_done {
                                    saw_done = true;
                                    yield Ok(StreamEvent::Done { stop_reason: None, usage: None });
                                }
                            }
                            "error" => {
                                let message = event
                                    .get("error")
                                    .and_then(|e| e.get("message"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown provider error");
                                yield Err(ProviderError::Http { status: 500, body: message.to_string() });
                            }
                            _ => {}
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let response = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .headers(self.headers()?)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(status_error(status.as_u16(), response).await);
        }

        let raw: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Decode(e.to_string()))?;
        let models = raw
            .get("data")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item.get("id").and_then(Value::as_str)?;
                        Some(ModelInfo {
                            id: id.to_string(),
                            display_name: id.to_string(),
                            context_window: 200_000,
                            max_output_tokens: 8_192,
                            supports_parallel_tools: true,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }
}
