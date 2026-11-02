//! OpenAI-compatible Chat Completions backend.
//!
//! This one adapter covers the majority of providers: OpenAI itself, MiniMax,
//! Groq, Together, DeepSeek, OpenRouter, Fireworks, and any local server
//! speaking the same shape (Ollama, LM Studio, vLLM, llama.cpp).
//!
//! The only per-provider differences are the base URL and the auth header, so
//! they are configuration rather than code.

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::anthropic::status_error;
use super::types::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ModelInfo, Role, StopReason,
    StreamEvent, ToolSpec, Usage,
};
use super::{EventStream, Provider, ProviderError, Result};

/// Where a llama.cpp server usually is.
///
/// `llama-server` listens on 8080 by default and serves the OpenAI shapes
/// under `/v1`.
pub const LLAMA_CPP_BASE_URL: &str = "http://localhost:8080/v1";

/// A server that speaks the OpenAI Chat Completions API.
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
    max_tokens: usize,
    /// Extra headers, e.g. `api-key` for Azure.
    extra_headers: Vec<(String, String)>,
}

impl OpenAiCompatProvider {
    pub fn new(api_key: Option<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        OpenAiCompatProvider {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
            model,
            max_tokens: 4096,
            extra_headers: Vec::new(),
        }
    }

    /// Override the API root. The `/chat/completions` suffix is appended.
    ///
    /// Ollama-style servers usually want `http://localhost:11434/v1`.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(key) = &self.api_key
            && !key.is_empty()
        {
            let value = HeaderValue::from_str(&super::bearer(key))
                .map_err(|_| ProviderError::Auth("invalid api key characters".into()))?;
            headers.insert("authorization", value);
        }
        for (name, value) in &self.extra_headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
        Ok(headers)
    }

    fn body(&self, request: &CompletionRequest, stream: bool) -> Value {
        let mut messages = Vec::new();

        // OpenAI takes the system prompt as a leading system message.
        if let Some(system) = &request.system {
            messages.push(json!({ "role": "system", "content": system }));
        }

        for message in &request.messages {
            messages.extend(encode_message(message));
        }

        let mut body = json!({
            "messages": messages,
        });
        // An unnamed model leaves the choice to the server, which is what
        // a llama.cpp server that was started with one model wants.
        let model = if request.model.is_empty() {
            self.model.as_str()
        } else {
            request.model.as_str()
        };
        if !model.is_empty() {
            body["model"] = json!(model);
        }

        if !request.tools.is_empty() {
            body["tools"] = json!(request.tools.iter().map(encode_tool).collect::<Vec<_>>());
            // Providers that do not support parallel calls reject the field.
            body["parallel_tool_calls"] = json!(true);
        }

        let max = request.max_output_tokens.unwrap_or(self.max_tokens);
        body["max_tokens"] = json!(max);
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
            .post(format!("{}/chat/completions", self.base_url))
            .headers(self.headers()?)
            .json(body)
            .send()
            .await?;

        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            Err(status_error(status.as_u16(), response).await)
        }
    }
}

/// Expand one neutral message into zero or more OpenAI messages.
///
/// The tool flow needs care: an assistant turn that made tool calls becomes an
/// assistant message with a `tool_calls` array, and each tool result becomes
/// its own `role: "tool"` message.
pub fn encode_message(message: &Message) -> Vec<Value> {
    let mut out = Vec::new();

    let tool_calls: Vec<Value> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": input.to_string(),
                },
            })),
            _ => None,
        })
        .collect();

    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let has_image = message
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Image { .. }));

    let role = match message.role {
        Role::Assistant => "assistant",
        Role::User | Role::System => "user",
        Role::Tool => "tool",
    };

    if has_image {
        // Images become multimodal content parts.
        let mut parts = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(json!({ "type": "text", "text": text }));
                }
                ContentBlock::Image { media_type, data } => {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{media_type};base64,{data}") },
                    }));
                }
                _ => {}
            }
        }
        out.push(json!({ "role": role, "content": parts }));
    } else if !text.is_empty() || !tool_calls.is_empty() {
        let mut msg = json!({ "role": role, "content": text });
        if !tool_calls.is_empty() {
            msg["tool_calls"] = json!(tool_calls);
        }
        out.push(msg);
    }

    // Tool results become standalone messages.
    for block in &message.content {
        if let ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } = block
        {
            out.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            }));
        }
    }

    out
}

fn encode_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        },
    })
}

fn map_stop_reason(stop: Option<&str>) -> Option<StopReason> {
    Some(match stop? {
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    })
}

fn parse_usage(raw: &Value) -> Option<Usage> {
    let input = raw
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = raw
        .get("completion_tokens")
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

/// Extract content blocks from a non-streaming response's message object.
fn parse_message(message: &Value) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        blocks.push(ContentBlock::Text {
            text: text.to_string(),
        });
    }

    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let function = call.get("function").cloned().unwrap_or(Value::Null);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // Arguments arrive as a JSON string and may be partial or empty.
            let input = match function.get("arguments").and_then(Value::as_str) {
                Some(args) if !args.trim().is_empty() => {
                    serde_json::from_str(args).unwrap_or_else(|_| json!({ "_raw": args }))
                }
                _ => json!({}),
            };
            blocks.push(ContentBlock::ToolUse { id, name, input });
        }
    }

    blocks
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        "openai"
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

        let message = raw
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .cloned()
            .unwrap_or(Value::Null);

        Ok(CompletionResponse {
            content: parse_message(&message),
            model: raw
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&self.model)
                .to_string(),
            stop_reason: raw
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("finish_reason"))
                .and_then(Value::as_str)
                .and_then(|s| map_stop_reason(Some(s))),
            usage: raw.get("usage").and_then(parse_usage),
        })
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        let body = self.body(request, true);
        let response = self.post(&body).await?;

        let mut buffer = String::new();
        // Tool calls stream as indexed deltas; we accumulate each index.
        let mut tool_names: Vec<String> = Vec::new();
        let mut tool_args: Vec<String> = Vec::new();
        let mut tool_ids: Vec<String> = Vec::new();

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();

            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(ProviderError::Network(e));
                        break;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buffer.find("\n\n") {
                    let frame = buffer[..pos].to_string();
                    buffer.drain(..pos + 2);

                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data.is_empty() || data == "[DONE]" { continue; }

                        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
                        let choice = event
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .cloned()
                            .unwrap_or(Value::Null);
                        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

                        if let Some(text) = delta.get("content").and_then(Value::as_str)
                            && !text.is_empty() {
                                yield Ok(StreamEvent::text(text));
                            }

                        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                            for call in calls {
                                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                                while tool_names.len() <= index {
                                    tool_names.push(String::new());
                                    tool_args.push(String::new());
                                    tool_ids.push(String::new());
                                }
                                if let Some(id) = call.get("id").and_then(Value::as_str) {
                                    tool_ids[index] = id.to_string();
                                }
                                let function = call.get("function").cloned().unwrap_or(Value::Null);

                                // The name arrives once, in the first delta for
                                // this call. Announce the call at that point so
                                // the consumer knows what is being built.
                                if let Some(name) = function.get("name").and_then(Value::as_str) {
                                    if !name.is_empty() && tool_names[index].is_empty() {
                                        tool_names[index] = name.to_string();
                                        yield Ok(StreamEvent::ToolCallStart {
                                            id: tool_ids[index].clone(),
                                            name: name.to_string(),
                                        });
                                    } else {
                                        tool_names[index] = name.to_string();
                                    }
                                }
                                if let Some(args) = function.get("arguments").and_then(Value::as_str) {
                                    tool_args[index].push_str(args);
                                    yield Ok(StreamEvent::ToolCallDelta {
                                        id: tool_ids[index].clone(),
                                        arguments: args.to_string(),
                                    });
                                }
                            }
                        }

                        if let Some(stop) = choice.get("finish_reason").and_then(Value::as_str) {
                            // The call is complete once the finish reason arrives.
                            for id in &tool_ids {
                                yield Ok(StreamEvent::ToolCallEnd { id: id.clone() });
                            }
                            let usage = event.get("usage").and_then(parse_usage);
                            yield Ok(StreamEvent::Done {
                                stop_reason: map_stop_reason(Some(stop)),
                                usage,
                            });
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
            .get(format!("{}/models", self.base_url))
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
                            context_window: 128_000,
                            max_output_tokens: self.max_tokens,
                            supports_parallel_tools: true,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }
}
