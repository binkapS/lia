//! Ollama backend.
//!
//! Ollama runs models on the machine itself, and offers the same models in
//! the cloud. The two speak the same API: a local server at
//! `http://localhost:11434`, the cloud at `https://ollama.com`, with
//! `/api/chat` underneath for the answers and `/api/tags` for the list of
//! models. Only the address and the key differ, so one provider covers
//! both.
//!
//! Ollama's own API is used rather than the OpenAI-shaped one, because it
//! says more: what the model was thinking, which tools it wants, how many
//! tokens it used, and how long the model should stay in memory afterwards.

use std::collections::HashMap;

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

/// Where a local Ollama server usually is.
pub const LOCAL_BASE_URL: &str = "http://localhost:11434";

/// Where Ollama's cloud is.
pub const CLOUD_BASE_URL: &str = "https://ollama.com";

/// The root of the API, with the paths people type by habit taken off.
///
/// Ollama's own API lives at the root of the server, but `/v1` and `/api`
/// are what people write down, so both are understood.
pub fn api_root(base_url: &str) -> &str {
    let base = base_url.trim_end_matches('/');
    for suffix in ["/api", "/v1"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            return stripped;
        }
    }
    base
}

/// A model served by Ollama, on this machine or in the cloud.
pub struct OllamaProvider {
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
    max_tokens: usize,
    /// How long the model should stay loaded after the answer.
    keep_alive: Option<String>,
    /// Whether the model should think before answering.
    think: Option<bool>,
}

impl OllamaProvider {
    /// A provider for a local Ollama server.
    ///
    /// A local server needs no key; the cloud does.
    pub fn new(api_key: Option<String>, model: impl Into<String>) -> Self {
        OllamaProvider {
            client: reqwest::Client::new(),
            api_key,
            base_url: LOCAL_BASE_URL.to_string(),
            model: model.into(),
            max_tokens: 4096,
            keep_alive: None,
            think: None,
        }
    }

    /// Point the provider at Ollama's cloud.
    pub fn cloud(mut self) -> Self {
        self.base_url = CLOUD_BASE_URL.to_string();
        self
    }

    /// Override the address of the server.
    ///
    /// Both the root of the API (`https://ollama.com`) and the paths people
    /// are used to typing (`https://ollama.com/v1`) are understood.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// How long the model should stay loaded after the answer.
    ///
    /// Ollama keeps a model in memory for a while by itself. Asking for
    /// longer means a local model is ready for the next question instead
    /// of being loaded again.
    pub fn with_keep_alive(mut self, keep_alive: Option<String>) -> Self {
        self.keep_alive = keep_alive.filter(|k| !k.trim().is_empty());
        self
    }

    /// Whether the model should think before answering.
    ///
    /// Left unset, the model decides for itself.
    pub fn with_think(mut self, think: Option<bool>) -> Self {
        self.think = think;
        self
    }

    /// The root of the API, with the paths people type by habit taken off.
    fn root(&self) -> &str {
        api_root(&self.base_url)
    }

    /// The address of one of the API's paths.
    fn url(&self, path: &str) -> String {
        format!("{}/api/{path}", self.root())
    }

    /// Whether this is Ollama's cloud rather than a server of one's own.
    fn is_cloud(&self) -> bool {
        let host = self
            .root()
            .split("//")
            .nth(1)
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("");
        host.eq_ignore_ascii_case("ollama.com") || host.ends_with(".ollama.com")
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
        Ok(headers)
    }

    /// The names to try for a model.
    ///
    /// A cloud model is named one way in the app and another in the API:
    /// `gemma4:cloud` in the first, `gemma4:31b` in the second. Dropping
    /// the suffix is often enough to find the model the caller meant, so it
    /// is tried as well.
    fn model_candidates(&self, model: &str) -> Vec<String> {
        let mut names = vec![model.to_string()];
        if self.is_cloud()
            && let Some(stripped) = model.strip_suffix(":cloud")
            && !stripped.is_empty()
        {
            names.push(stripped.to_string());
        }
        names
    }

    /// The request Ollama expects.
    fn body(&self, request: &CompletionRequest, model: &str, stream: bool) -> Value {
        let names = tool_names(&request.messages);
        let mut messages = Vec::new();

        // Ollama takes the system prompt as a message of its own.
        if let Some(system) = &request.system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        for message in &request.messages {
            messages.extend(encode_message(message, &names));
        }

        let mut options = json!({
            "num_predict": request.max_output_tokens.unwrap_or(self.max_tokens),
        });
        if let Some(temperature) = request.temperature {
            options["temperature"] = json!(temperature);
        }

        let mut body = json!({
            "model": model,
            "messages": messages,
            "options": options,
        });

        if !request.tools.is_empty() {
            body["tools"] = json!(request.tools.iter().map(encode_tool).collect::<Vec<_>>());
        }
        if let Some(keep_alive) = &self.keep_alive {
            body["keep_alive"] = json!(keep_alive);
        }
        if let Some(think) = self.think {
            body["think"] = json!(think);
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
            .post(self.url("chat"))
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

/// Expand one neutral message into the messages Ollama expects.
///
/// A tool result is a message of its own, naming the tool it answers, and
/// the tools the model asked for go along with the assistant's turn.
pub fn encode_message(message: &Message, names: &HashMap<String, String>) -> Vec<Value> {
    let mut out = Vec::new();

    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let thinking: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Thinking { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let role = match message.role {
        Role::Assistant => "assistant",
        Role::User => "user",
        Role::System => "system",
        Role::Tool => "tool",
    };

    if message.role == Role::Tool {
        for block in &message.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = block
            {
                // Ollama names the tool rather than the call, and the call
                // ids it is given carry no name of their own.
                let name = names
                    .get(tool_use_id)
                    .map(String::as_str)
                    .unwrap_or_else(|| name_from_id(tool_use_id));
                out.push(json!({
                    "role": "tool",
                    "tool_name": name,
                    "content": content,
                }));
            }
        }
        return out;
    }

    let mut msg = json!({ "role": role, "content": text });

    if !thinking.is_empty() {
        msg["thinking"] = json!(thinking);
    }

    // Pictures go along as bare base64, without the data: prefix.
    let images: Vec<String> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { data, .. } => Some(data.clone()),
            _ => None,
        })
        .collect();
    if !images.is_empty() {
        msg["images"] = json!(images);
    }

    // The tools the model asked for.
    let mut calls = Vec::new();
    for block in &message.content {
        if let ContentBlock::ToolUse { name, input, .. } = block {
            calls.push(json!({
                "type": "function",
                "function": {
                    "index": calls.len(),
                    "name": name,
                    "arguments": input,
                },
            }));
        }
    }
    if !calls.is_empty() {
        msg["tool_calls"] = json!(calls);
    }

    out.push(msg);
    out
}

/// A tool, in the shape Ollama expects.
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

/// The names of the tools the model asked for, by the ids they were given.
fn tool_names(messages: &[Message]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for message in messages {
        for (id, name, _) in message.tool_uses() {
            names.insert(id.to_string(), name.to_string());
        }
    }
    names
}

/// The tool a call id names, when the conversation does not say.
///
/// The ids are minted as `call_<something>_<name>`, so the name is what
/// follows the first underscore.
fn name_from_id(id: &str) -> &str {
    id.strip_prefix("call_")
        .and_then(|rest| rest.split_once('_').map(|(_, name)| name))
        .filter(|name| !name.is_empty())
        .unwrap_or(id)
}

/// Give a tool call an id of its own.
fn mint_id(name: &str) -> String {
    let token = uuid::Uuid::new_v4().simple().to_string();
    format!("call_{}_{}", &token[..8], name)
}

/// Why the model stopped.
fn map_stop_reason(stop: Option<&str>, tool_use: bool) -> Option<StopReason> {
    if tool_use {
        return Some(StopReason::ToolUse);
    }
    Some(match stop.unwrap_or("stop") {
        "length" => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    })
}

/// The tokens a turn used, as Ollama counts them.
fn parse_usage(raw: &Value) -> Option<Usage> {
    let input = raw
        .get("prompt_eval_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = raw.get("eval_count").and_then(Value::as_u64).unwrap_or(0);
    if input == 0 && output == 0 {
        None
    } else {
        Some(Usage {
            input_tokens: input,
            output_tokens: output,
        })
    }
}

/// The blocks of an answer, from the message Ollama sends back.
fn parse_message(message: &Value) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    if let Some(thinking) = message.get("thinking").and_then(Value::as_str)
        && !thinking.is_empty()
    {
        blocks.push(ContentBlock::Thinking {
            text: thinking.to_string(),
        });
    }

    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        blocks.push(ContentBlock::Text {
            text: text.to_string(),
        });
    }

    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let function = call.get("function").cloned().unwrap_or(Value::Null);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // The arguments are an object, though a server that sends them as
        // a string is not to be blamed.
        let input = match function.get("arguments") {
            Some(Value::String(raw)) => {
                serde_json::from_str(raw).unwrap_or_else(|_| json!({ "_raw": raw.clone() }))
            }
            Some(value) => value.clone(),
            None => json!({}),
        };
        let id = mint_id(&name);
        blocks.push(ContentBlock::ToolUse { id, name, input });
    }

    blocks
}

/// The events one line of the stream carries.
///
/// Ollama streams as one JSON object per line, and says when it is done.
fn events_of(line: &str, tool_use: &mut bool) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
        return events;
    };
    let message = event.get("message").cloned().unwrap_or(Value::Null);

    if let Some(thinking) = message.get("thinking").and_then(Value::as_str)
        && !thinking.is_empty()
    {
        events.push(StreamEvent::Thinking(thinking.to_string()));
    }

    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        events.push(StreamEvent::Text(text.to_string()));
    }

    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let function = call.get("function").cloned().unwrap_or(Value::Null);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = match function.get("arguments") {
            Some(Value::String(raw)) => raw.clone(),
            Some(value) => value.to_string(),
            None => "{}".to_string(),
        };
        let id = mint_id(&name);
        *tool_use = true;
        events.push(StreamEvent::ToolCallStart {
            id: id.clone(),
            name,
        });
        events.push(StreamEvent::ToolCallDelta {
            id: id.clone(),
            arguments,
        });
        events.push(StreamEvent::ToolCallEnd { id });
    }

    if event.get("done").and_then(Value::as_bool).unwrap_or(false) {
        events.push(StreamEvent::Done {
            stop_reason: map_stop_reason(
                event.get("done_reason").and_then(Value::as_str),
                *tool_use,
            ),
            usage: parse_usage(&event),
        });
    }

    events
}

/// Whether the error says the model is not there.
fn is_missing_model(error: &ProviderError) -> bool {
    matches!(error, ProviderError::ModelNotFound(_))
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        if self.is_cloud() {
            "ollama-cloud"
        } else {
            "ollama"
        }
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let model = if request.model.is_empty() {
            self.model.clone()
        } else {
            request.model.clone()
        };

        let mut missing = None;
        for candidate in self.model_candidates(&model) {
            let body = self.body(request, &candidate, false);
            let response = match self.post(&body).await {
                Ok(response) => response,
                Err(e) if is_missing_model(&e) => {
                    missing = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            };

            let raw: Value = response
                .json()
                .await
                .map_err(|e| ProviderError::Decode(e.to_string()))?;

            let message = raw.get("message").cloned().unwrap_or(Value::Null);
            let content = parse_message(&message);
            let tool_use = content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));

            return Ok(CompletionResponse {
                content,
                model: raw
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.model)
                    .to_string(),
                stop_reason: map_stop_reason(
                    raw.get("done_reason").and_then(Value::as_str),
                    tool_use,
                ),
                usage: parse_usage(&raw),
            });
        }

        Err(missing.unwrap_or_else(|| ProviderError::ModelNotFound(model)))
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        let model = if request.model.is_empty() {
            self.model.clone()
        } else {
            request.model.clone()
        };

        let mut response = None;
        let mut missing = None;
        for candidate in self.model_candidates(&model) {
            let body = self.body(request, &candidate, true);
            match self.post(&body).await {
                Ok(answer) => {
                    response = Some(answer);
                    break;
                }
                Err(e) if is_missing_model(&e) => missing = Some(e),
                Err(e) => return Err(e),
            }
        }

        let response = match response {
            Some(response) => response,
            None => return Err(missing.unwrap_or_else(|| ProviderError::ModelNotFound(model))),
        };

        let stream = async_stream::stream! {
            let mut bytes = response.bytes_stream();
            let mut buffer = String::new();
            let mut tool_use = false;

            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(ProviderError::Network(e));
                        break;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Ollama sends one JSON object per line.
                while let Some(at) = buffer.find('\n') {
                    let line = buffer[..at].to_string();
                    buffer.drain(..at + 1);
                    for event in events_of(&line, &mut tool_use) {
                        yield Ok(event);
                    }
                }
            }

            // The last line may arrive without a newline ending it.
            for event in events_of(&buffer, &mut tool_use) {
                yield Ok(event);
            }
        };

        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let response = self
            .client
            .get(self.url("tags"))
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
            .get("models")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item
                            .get("name")
                            .or_else(|| item.get("model"))
                            .and_then(Value::as_str)?;
                        Some(ModelInfo {
                            id: id.to_string(),
                            display_name: id.to_string(),
                            // Ollama does not say how much context a model
                            // has; the number is a common one, and nothing
                            // here decides anything by it.
                            context_window: 32_768,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::Message;

    /// A message, with the tools the model asked for.
    fn a_turn() -> Vec<Message> {
        vec![
            Message::user("what is in the file?"),
            Message::assistant_tool_use(vec![ContentBlock::ToolUse {
                id: "call_1a2b3c4d_read_file".into(),
                name: "read_file".into(),
                input: json!({ "path": "notes.txt" }),
            }]),
            Message::tool_result("call_1a2b3c4d_read_file", "the contents"),
        ]
    }

    #[test]
    fn a_result_names_the_tool_it_answers() {
        let messages = a_turn();
        let names = tool_names(&messages);

        let encoded = encode_message(&messages[2], &names);
        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0]["role"], "tool");
        assert_eq!(encoded[0]["tool_name"], "read_file");
        assert_eq!(encoded[0]["content"], "the contents");
    }

    #[test]
    fn a_result_is_named_by_its_id_when_the_turn_does_not_say() {
        let message = Message::tool_result("call_1a2b3c4d_read_file", "the contents");
        let encoded = encode_message(&message, &HashMap::new());
        assert_eq!(encoded[0]["tool_name"], "read_file");
    }

    #[test]
    fn the_tools_the_model_asked_for_go_with_its_turn() {
        let messages = a_turn();
        let names = tool_names(&messages);

        let encoded = encode_message(&messages[1], &names);
        assert_eq!(encoded[0]["role"], "assistant");
        let calls = encoded[0]["tool_calls"].as_array().expect("the calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "read_file");
        assert_eq!(calls[0]["function"]["arguments"]["path"], "notes.txt");
    }

    #[test]
    fn pictures_go_along_as_bare_base64() {
        let message = Message::user_with_images(
            "what is this?",
            vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
        );
        let encoded = encode_message(&message, &HashMap::new());
        assert_eq!(encoded[0]["images"][0], "aGVsbG8=");
    }

    #[test]
    fn the_thinking_is_kept_apart_from_the_answer() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking { text: "hmm".into() },
                ContentBlock::Text {
                    text: "the answer".into(),
                },
            ],
        };
        let encoded = encode_message(&message, &HashMap::new());
        assert_eq!(encoded[0]["thinking"], "hmm");
        assert_eq!(encoded[0]["content"], "the answer");
    }

    #[test]
    fn an_answer_is_read_into_its_parts() {
        let message = json!({
            "role": "assistant",
            "thinking": "let me look",
            "content": "here it is",
            "tool_calls": [{
                "type": "function",
                "function": { "index": 0, "name": "read_file", "arguments": { "path": "a" } },
            }],
        });

        let blocks = parse_message(&message);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], ContentBlock::Thinking { text } if text == "let me look"));
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text == "here it is"));
        let ContentBlock::ToolUse { name, input, .. } = &blocks[2] else {
            panic!("the tool call is missing");
        };
        assert_eq!(name, "read_file");
        assert_eq!(input["path"], "a");
    }

    #[test]
    fn a_stream_line_becomes_events() {
        let mut tool_use = false;
        let events = events_of(
            r#"{"message":{"role":"assistant","content":"hello"},"done":false}"#,
            &mut tool_use,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Text("hello".into()));
    }

    #[test]
    fn the_end_of_a_stream_says_what_was_used() {
        let mut tool_use = false;
        let events = events_of(
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":11,"eval_count":18}"#,
            &mut tool_use,
        );
        let StreamEvent::Done { stop_reason, usage } = events[0] else {
            panic!("the end of the stream was not reported");
        };
        assert_eq!(stop_reason, Some(StopReason::EndTurn));
        let usage = usage.expect("the tokens were counted");
        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 18);
    }

    #[test]
    fn the_paths_people_type_are_understood() {
        assert_eq!(api_root("https://ollama.com/"), "https://ollama.com");
        assert_eq!(
            api_root("http://localhost:11434/v1"),
            "http://localhost:11434"
        );

        let provider =
            OllamaProvider::new(None, "llama3.2").with_base_url("http://localhost:11434/v1");
        assert_eq!(provider.url("chat"), "http://localhost:11434/api/chat");

        let provider =
            OllamaProvider::new(None, "gemma4:31b").with_base_url("https://ollama.com/api");
        assert_eq!(provider.url("tags"), "https://ollama.com/api/tags");
    }

    #[test]
    fn the_cloud_is_told_from_a_server_of_ones_own() {
        let local = OllamaProvider::new(None, "llama3.2");
        assert_eq!(local.name(), "ollama");
        assert!(!local.is_cloud());

        let cloud = OllamaProvider::new(None, "gemma4:31b").cloud();
        assert_eq!(cloud.name(), "ollama-cloud");
        assert!(cloud.is_cloud());
    }

    #[test]
    fn a_cloud_model_is_tried_with_its_suffix_and_without() {
        let cloud = OllamaProvider::new(None, "gemma4:cloud").cloud();
        let names = cloud.model_candidates("gemma4:cloud");
        assert_eq!(names, vec!["gemma4:cloud", "gemma4"]);

        // A local server knows the ":cloud" names, and is left alone.
        let local = OllamaProvider::new(None, "gemma4:cloud");
        assert_eq!(local.model_candidates("gemma4:cloud"), vec!["gemma4:cloud"]);
    }
}
