//! The tool that lets the assistant speak first.
//!
//! A usual reply answers a message: the channel hands it over, the turn
//! runs, and the answer goes back through the same channel. A message the
//! assistant starts on its own — "tell me when the build is done" said in
//! the terminal, a job that finishes while nobody is looking — has no
//! message to answer, so it needs its own way out: the [`SendMessageTool`]
//! addresses a channel and a conversation and pushes the words out.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Result, Tool, ToolContext, ToolError, ToolOutput};
use crate::channel::{ChannelHub, OutboundMessage};
use crate::schedule::Target;

/// Sends a message to a conversation, without anything being said first.
pub struct SendMessageTool {
    hub: ChannelHub,
}

impl SendMessageTool {
    /// A tool that sends through the channels in `hub`.
    pub fn new(hub: ChannelHub) -> SendMessageTool {
        SendMessageTool { hub }
    }

    /// Work out where the message goes.
    ///
    /// The model may name the chat, or leave it out to send to the chat
    /// the turn is running in. A session id is `channel:conversation`, so
    /// the turn's own chat is worked out from that.
    fn pick_target(&self, args: &Value, session: &str) -> Option<Target> {
        let named_channel = args
            .get("channel")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let named_conversation = args
            .get("conversation")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty());

        match (named_channel, named_conversation) {
            (Some(channel), Some(conversation)) => {
                Some(Target::Chat {
                    channel: channel.to_string(),
                    conversation: conversation.to_string(),
                })
            }
            // A turn in a chat needs no name: this is where it was asked.
            (None, None) => match Target::for_session(session) {
                Target::Session => None,
                chat => Some(chat),
            },
            _ => None,
        }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to a chat without anything being said first. Use it \
         to report back on work the user asked to be told about, or to send \
         a note to another conversation by name. Leave `channel` and \
         `conversation` out to send to the chat this turn is running in; \
         name both to send somewhere else, e.g. channel \"telegram\" and \
         conversation \"123456789\". The message is delivered even if the \
         user is not watching, so keep it short and self-contained."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "What to send, as one message the recipient will read on its own."
                },
                "channel": {
                    "type": "string",
                    "description": "Which platform, e.g. \"telegram\", \"discord\", \"slack\" or \"whatsapp\". Needed together with `conversation`."
                },
                "conversation": {
                    "type": "string",
                    "description": "Which chat on that platform: the id used in session names, e.g. 123456789. Needed together with `channel`."
                }
            },
            "required": ["text"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs {
                tool: "send_message".into(),
                reason: "what should be sent? give the text".into(),
            })?;
        if text.trim().is_empty() {
            return Ok(ToolOutput::error(
                "there was no message to send: the text is empty.",
            ));
        }

        let session = args
            .get("__session")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(target) = self.pick_target(&args, session) else {
            // A turn in the terminal or the gateway has no chat to send to.
            return Ok(ToolOutput::error(
                "there is nowhere to send: this conversation is not a chat. \
                 Name the channel and the conversation to send to."
                    .to_string(),
            ));
        };

        let Target::Chat { channel, conversation } = target else {
            return Ok(ToolOutput::error(
                "there is nowhere to send: this conversation is not a chat. \
                 Name the channel and the conversation to send to."
                    .to_string(),
            ));
        };

        let Some(running) = self.hub.get(&channel) else {
            let names = self.hub.names();
            let hint = if names.is_empty() {
                "no channels are running".to_string()
            } else {
                format!("the ones that are running: {}", names.join(", "))
            };
            return Ok(ToolOutput::error(format!(
                "the channel '{channel}' is not running — {hint}."
            )));
        };

        match running.send(&OutboundMessage::new(&conversation, text)).await {
            Ok(()) => Ok(ToolOutput::ok(format!(
                "sent to {conversation} on {}.",
                running.name()
            ))),
            Err(e) => Ok(ToolOutput::error(format!(
                "the message could not be sent to {conversation} on {}: {e}",
                running.name()
            ))),
        }
    }

    /// The session comes from the turn, not from the model.
    fn with_context(&self, mut args: Value, context: &ToolContext) -> Value {
        if let Some(object) = args.as_object_mut() {
            object.insert(
                "__session".into(),
                Value::String(context.session_id.clone()),
            );
        }
        args
    }

    /// A message the assistant starts by itself reaches people who are not
    /// looking — the words leave the conversation they were written in, and
    /// they may carry whatever the turn has learned. It is asked about,
    /// unless the user has said otherwise.
    fn requires_approval(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Channel, InboundSender, OutboundMessage, OutboundSender};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// A channel that keeps what it was told to send.
    struct MockChannel {
        name: &'static str,
        sent: Mutex<Vec<OutboundMessage>>,
    }

    impl MockChannel {
        fn new(name: &'static str) -> Arc<MockChannel> {
            Arc::new(MockChannel {
                name,
                sent: Mutex::new(Vec::new()),
            })
        }

        fn sent(&self) -> Vec<OutboundMessage> {
            self.sent.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            self.name
        }

        async fn run(
            &self,
            _inbound: InboundSender,
            _outbound: OutboundSender,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn send(&self, message: &OutboundMessage) -> anyhow::Result<()> {
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }
    }

    fn tool_with(hub: &ChannelHub) -> SendMessageTool {
        SendMessageTool::new(hub.clone())
    }

    /// A turn in a chat sends to that chat when no target is named.
    #[tokio::test]
    async fn a_turn_in_a_chat_sends_to_that_chat() {
        let hub = ChannelHub::new();
        let telegram = MockChannel::new("telegram");
        hub.add(telegram.clone());
        let tool = tool_with(&hub);

        let args = with_session(
            json!({ "text": "the build is done" }),
            "telegram:123456789",
        );
        let outcome = tool.run(args).await.expect("the tool answers");

        assert!(!outcome.is_error);
        assert_eq!(
            telegram.sent()[0].conversation,
            "123456789",
            "the message went to the chat the turn runs in"
        );
    }

    /// A turn outside a chat with no target named goes nowhere.
    #[tokio::test]
    async fn a_session_without_a_chat_sends_nowhere() {
        let hub = ChannelHub::new();
        let telegram = MockChannel::new("telegram");
        hub.add(telegram.clone());
        let tool = tool_with(&hub);

        let outcome = tool
            .run(with_session(json!({ "text": "hello" }), "e78e4e0d-92c5-47e0-9b2b-9cd5f4b6a1a2"))
            .await
            .expect("the tool answers");

        assert!(outcome.is_error, "nowhere was named to send to");
        assert!(
            telegram.sent().is_empty(),
            "a message was sent without a target"
        );
    }

    /// Naming both the channel and the conversation sends there, even from
    /// a session that has no chat of its own.
    #[tokio::test]
    async fn a_named_target_is_sent_to_by_name() {
        let hub = ChannelHub::new();
        let whatsapp = MockChannel::new("whatsapp");
        hub.add(whatsapp.clone());
        let tool = tool_with(&hub);

        let args = with_session(
            json!({ "text": "the job finished", "channel": "whatsapp", "conversation": "15551234567" }),
            "e78e4e0d-92c5-47e0-9b2b-9cd5f4b6a1a2",
        );
        let outcome = tool.run(args).await.expect("the tool answers");

        assert!(!outcome.is_error);
        assert_eq!(whatsapp.sent()[0].conversation, "15551234567");
        assert_eq!(whatsapp.sent()[0].text, "the job finished");
    }

    /// A channel that is not running is not silently claimed delivered.
    #[tokio::test]
    async fn an_unknown_channel_is_an_error() {
        let hub = ChannelHub::new();
        hub.add(MockChannel::new("telegram"));
        let tool = tool_with(&hub);

        let args = with_session(
            json!({ "text": "hi", "channel": "rocketchat", "conversation": "1" }),
            "telegram:2",
        );
        let outcome = tool.run(args).await.expect("the tool answers");

        assert!(outcome.is_error);
        assert!(
            outcome.content.contains("telegram"),
            "the error says which channels do run: {}",
            outcome.content
        );
    }

    /// A channel and conversation must come together: one without the
    /// other is an argument the model cannot have meant.
    #[tokio::test]
    async fn half_a_target_is_refused() {
        let hub = ChannelHub::new();
        let telegram = MockChannel::new("telegram");
        hub.add(telegram.clone());
        let tool = tool_with(&hub);

        let args = with_session(
            json!({ "text": "hi", "channel": "telegram" }),
            "telegram:123456789",
        );
        let outcome = tool.run(args).await;

        // The half-notion is caught before anything is sent.
        assert!(
            telegram.sent().is_empty(),
            "half an address still sent the message"
        );
        let _ = outcome;
    }

    /// Empty text is refused rather than sent as an accident.
    #[tokio::test]
    async fn empty_text_is_refused() {
        let hub = ChannelHub::new();
        let telegram = MockChannel::new("telegram");
        hub.add(telegram.clone());
        let tool = tool_with(&hub);

        let outcome = tool
            .run(with_session(json!({ "text": "   " }), "telegram:123456789"))
            .await
            .expect("the tool answers");

        assert!(outcome.is_error, "an empty message was not sent");
        assert!(telegram.sent().is_empty());
    }

    /// The tool asks before it sends.
    #[test]
    fn a_send_is_asked_about() {
        let tool = SendMessageTool::new(ChannelHub::new());
        assert!(tool.requires_approval());
    }

    /// The args with the turn's session attached, the way the agent does.
    fn with_session(args: Value, session: &str) -> Value {
        let tool = SendMessageTool::new(ChannelHub::new());
        let context = ToolContext {
            tainted: false,
            user_text: String::new(),
            session_id: session.to_string(),
        };
        tool.with_context(args, &context)
    }
}
