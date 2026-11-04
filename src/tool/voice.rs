//! Listening and speaking as tools.
//!
//! `listen` records from the microphone and hands the words back, so the
//! assistant can be spoken to. `speak` says the given text aloud.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Result, Tool, ToolError, ToolOutput};
use crate::voice::{RecordOptions, Voice};

/// Record from the microphone and return what was said.
pub struct ListenTool {
    voice: Arc<Voice>,
    max_seconds: u64,
}

impl ListenTool {
    pub fn new(voice: Arc<Voice>) -> Self {
        ListenTool {
            voice,
            max_seconds: 60,
        }
    }

    /// How long a recording may last.
    pub fn with_max_seconds(mut self, max_seconds: u64) -> Self {
        self.max_seconds = max_seconds;
        self
    }
}

#[async_trait]
impl Tool for ListenTool {
    fn name(&self) -> &str {
        "listen"
    }

    fn description(&self) -> &str {
        "Record from the microphone and return what was said, as text. \
         Use it when the user speaks to you, or when you need to hear a \
         sound that is not in the conversation. The recording ends when the \
         user stops talking, or when the time runs out."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_seconds": {
                    "type": "integer",
                    "description": "How long to record for, in seconds."
                }
            }
        })
    }

    fn requires_approval(&self) -> bool {
        // The microphone is the user's own ear and mouth, so the user is
        // asked before it is opened.
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let max_seconds = args
            .get("max_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(self.max_seconds)
            .clamp(1, 600);

        let text = self
            .voice
            .listen(RecordOptions {
                max_seconds,
                silence: None,
            })
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(ToolOutput {
                content: "Nothing was said.".to_string(),
                is_error: false,
                metadata: Some(json!({ "heard_speech": false })),
            });
        }

        Ok(ToolOutput {
            content: text.clone(),
            is_error: false,
            metadata: Some(json!({
                "heard_speech": true,
                "source": "microphone",
            })),
        })
    }
}

/// Say something aloud.
pub struct SpeakTool {
    voice: Arc<Voice>,
}

impl SpeakTool {
    pub fn new(voice: Arc<Voice>) -> Self {
        SpeakTool { voice }
    }
}

#[async_trait]
impl Tool for SpeakTool {
    fn name(&self) -> &str {
        "speak"
    }

    fn description(&self) -> &str {
        "Say a piece of text out loud on this machine. Use it to answer the \
         user when they are listening rather than reading."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The words to say."
                }
            },
            "required": ["text"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let text =
            args.get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'text'".into(),
                })?;

        if text.trim().is_empty() {
            return Ok(ToolOutput::ok("there was nothing to say"));
        }

        self.voice
            .say(text)
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        Ok(ToolOutput {
            content: format!("Said: {text}"),
            is_error: false,
            metadata: Some(json!({ "spoken": true })),
        })
    }
}

/// Report voice listening/speaking health.
pub struct VoiceHealthTool {
    voice: Arc<Voice>,
}

impl VoiceHealthTool {
    pub fn new(voice: Arc<Voice>) -> Self {
        VoiceHealthTool { voice }
    }
}

#[async_trait]
impl Tool for VoiceHealthTool {
    fn name(&self) -> &str {
        "voice_health"
    }

    fn description(&self) -> &str {
        "Check whether listening and speaking are available."
    }

    fn parameters(&self) -> Value {
        json!({ "type":"object", "properties":{} })
    }

    async fn run(&self, _args: Value) -> Result<ToolOutput> {
        let content = format!(
            "listen: {}\nspeak: {}",
            self.voice.can_listen(),
            self.voice.can_speak()
        );
        let metadata = json!({
            "listen": self.voice.can_listen(),
            "speak": self.voice.can_speak(),
        });
        Ok(ToolOutput::ok(content).with_metadata(metadata))
    }
}

pub type VoiceStatusTool = VoiceHealthTool;
