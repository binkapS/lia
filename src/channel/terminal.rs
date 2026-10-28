//! Channels that read from stdin and write to stdout.
//!
//! These are what the non-interactive CLI modes use, and they double as a
//! simple way to pipe work through the assistant.

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use super::{Channel, InboundMessage, InboundSender, OutboundMessage, OutboundSender};

/// Reads lines from stdin and forwards them to the assistant.
pub struct TerminalChannel {
    /// The sender id recorded on each inbound message.
    sender: String,
}

impl TerminalChannel {
    pub fn new() -> Self {
        TerminalChannel {
            sender: "stdin".to_string(),
        }
    }

    pub fn with_sender(sender: impl Into<String>) -> Self {
        TerminalChannel {
            sender: sender.into(),
        }
    }
}

impl Default for TerminalChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Channel for TerminalChannel {
    fn name(&self) -> &str {
        "terminal"
    }

    async fn run(&self, inbound: InboundSender, _outbound: OutboundSender) -> anyhow::Result<()> {
        let stdin = BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();

        while let Some(line) = lines.next_line().await? {
            let text = line.trim().to_string();
            if text.is_empty() {
                continue;
            }
            let message = InboundMessage::new(self.name(), &self.sender, text, "stdin");
            if inbound.send(message).await.is_err() {
                break;
            }
        }
        Ok(())
    }
}

/// A channel that delivers one fixed message and then stops.
///
/// Used by the one-shot CLI mode, where the prompt comes from a flag rather
/// than a stream.
pub struct OneShotChannel {
    message: String,
}

impl OneShotChannel {
    pub fn new(message: impl Into<String>) -> Self {
        OneShotChannel {
            message: message.into(),
        }
    }
}

#[async_trait]
impl Channel for OneShotChannel {
    fn name(&self) -> &str {
        "oneshot"
    }

    async fn run(&self, inbound: InboundSender, _outbound: OutboundSender) -> anyhow::Result<()> {
        let message = InboundMessage::new(self.name(), "cli", self.message.clone(), "cli");
        let _ = inbound.send(message).await;
        Ok(())
    }
}

/// Drain outbound messages and print them to stdout.
pub async fn print_outbound(mut outbound: mpsc::Receiver<OutboundMessage>) {
    use std::io::Write;
    while let Some(message) = outbound.recv().await {
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "{}", message.text);
        let _ = stdout.flush();
    }
}
