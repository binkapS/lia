//! Tests for the provider wire encodings.
//!
//! These check that the neutral message types translate correctly into each
//! provider's wire format — the part that is easy to get subtly wrong and
//! impossible to notice until a model misbehaves.

use lia::provider::types::{CompletionRequest, ContentBlock, Message, Role, ToolSpec};
use serde_json::json;

fn tool_spec() -> ToolSpec {
    ToolSpec::new(
        "read_file",
        "Read a file",
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    )
}

#[test]
fn openai_encodes_tool_use_as_tool_calls() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::text("Let me look."),
            ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: json!({ "path": "a.txt" }),
            },
        ],
    };

    let encoded = lia::provider::openai_compat::encode_message(&message);
    assert_eq!(encoded.len(), 1);

    let msg = &encoded[0];
    assert_eq!(msg["role"], "assistant");
    assert_eq!(msg["content"], "Let me look.");

    let calls = msg["tool_calls"].as_array().expect("tool_calls present");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(calls[0]["type"], "function");
    assert_eq!(calls[0]["function"]["name"], "read_file");

    // Arguments are a JSON *string* in the OpenAI format, not an object.
    let args = calls[0]["function"]["arguments"]
        .as_str()
        .expect("string args");
    let parsed: serde_json::Value = serde_json::from_str(args).unwrap();
    assert_eq!(parsed["path"], "a.txt");
}

#[test]
fn openai_encodes_tool_result_as_its_own_message() {
    let message = Message::tool_result("call_1", "file contents");
    let encoded = lia::provider::openai_compat::encode_message(&message);

    assert_eq!(encoded.len(), 1);
    assert_eq!(encoded[0]["role"], "tool");
    assert_eq!(encoded[0]["tool_call_id"], "call_1");
    assert_eq!(encoded[0]["content"], "file contents");
}

#[test]
fn anthropic_encodes_tool_result_inside_a_user_message() {
    // Anthropic carries tool results as a content block in a user turn.
    let message = Message::tool_result("call_1", "file contents");
    let encoded = lia::provider::anthropic::encode_message(&message);

    assert_eq!(encoded["role"], "user");
    let content = encoded["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["tool_use_id"], "call_1");
    assert_eq!(content[0]["content"], "file contents");
}

#[test]
fn anthropic_encodes_tool_use_as_a_block() {
    let message = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "read_file".into(),
            input: json!({ "path": "a.txt" }),
        }],
    };
    let encoded = lia::provider::anthropic::encode_message(&message);

    assert_eq!(encoded["role"], "assistant");
    let content = encoded["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "tool_use");
    assert_eq!(content[0]["name"], "read_file");
    assert_eq!(content[0]["input"]["path"], "a.txt");
}

#[test]
fn anthropic_encodes_images_as_base64_sources() {
    let message = Message {
        role: Role::User,
        content: vec![ContentBlock::Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        }],
    };
    let encoded = lia::provider::anthropic::encode_message(&message);

    let content = encoded["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert_eq!(content[0]["source"]["data"], "AAAA");
}

#[test]
fn openai_encodes_images_as_data_urls() {
    let message = Message {
        role: Role::User,
        content: vec![ContentBlock::Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        }],
    };
    let encoded = lia::provider::openai_compat::encode_message(&message);

    let content = encoded[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "image_url");
    assert_eq!(content[0]["image_url"]["url"], "data:image/png;base64,AAAA");
}

#[test]
fn request_keeps_the_system_prompt_separate() {
    // Both providers take the system prompt outside the message list.
    let request = CompletionRequest {
        model: "test".into(),
        system: Some("be helpful".into()),
        messages: vec![Message::user("hi")],
        tools: vec![tool_spec()],
        ..Default::default()
    };

    assert_eq!(request.system.as_deref(), Some("be helpful"));
    assert_eq!(request.messages[0].role, Role::User);
    assert_eq!(request.messages[0].text(), "hi");
}
