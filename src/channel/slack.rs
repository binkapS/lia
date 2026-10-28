//! Slack channel adapter.
//!
//! Uses the Web API and polls the conversations it has been allowed to speak
//! in. Polling rather than the Events API keeps the adapter to one HTTP
//! client and lets it work on a machine that cannot be reached from the
//! internet.
//!
//! The token comes from `SLACK_BOT_TOKEN` and is never written to disk. Only
//! the conversations listed in `SLACK_ALLOWED_CHANNELS` are polled, and an
//! empty list means nobody can talk to the assistant: a bot that answers
//! anyone would let them run commands on this machine.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{
    Channel, InboundMessage, InboundSender, OutboundMessage, OutboundSender, Seen, is_allowed_url,
    split_message,
};

/// The Slack API root.
const API: &str = "https://slack.com/api";

/// Slack allows a good deal more than this, but a message that long is
/// unreadable in most clients anyway.
const MESSAGE_LIMIT: usize = 3000;

/// How often the conversations are polled, in seconds.
const POLL_SECONDS: u64 = 3;

/// A message on a Slack conversation.
#[derive(Debug, Clone, Deserialize)]
pub struct SlackMessage {
    /// The type of the message; only `message` is one.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The id of the user who wrote it.
    #[serde(default)]
    pub user: Option<String>,
    /// The text of the message.
    #[serde(default)]
    pub text: String,
    /// The time it was sent, as Slack's `seconds.microseconds`.
    #[serde(default)]
    pub ts: String,
    /// Set when a bot wrote the message.
    #[serde(default)]
    pub bot_id: Option<String>,
    /// The files that came with the message.
    #[serde(default)]
    pub files: Vec<SlackFile>,
}

/// A file that came with a Slack message.
#[derive(Debug, Clone, Deserialize)]
pub struct SlackFile {
    /// The id of the file, when the platform gave one.
    #[serde(default)]
    pub id: Option<String>,
    /// The name of the file.
    #[serde(default)]
    pub name: Option<String>,
    /// What kind of file it is.
    #[serde(default)]
    pub mimetype: Option<String>,
    /// Where the file can be fetched from. It needs the bot's token.
    #[serde(default)]
    pub url_private: Option<String>,
}

/// The user the bot is.
#[derive(Debug, Clone, Deserialize)]
struct SlackUser {
    id: String,
}

/// The Slack adapter.
#[derive(Clone)]
pub struct SlackChannel {
    token: String,
    channels: Vec<String>,
    users: HashSet<String>,
    client: reqwest::Client,
    /// The API root, so a proxy or a test can stand in for Slack.
    api_root: String,
    /// How often the conversations are polled.
    poll_seconds: u64,
    /// The bot's own user id, so it does not answer itself.
    bot_id: tokio::sync::OnceCell<String>,
    /// How far each conversation has been read.
    seen: Seen,
    /// Set when the adapter should stop.
    shutdown: Arc<tokio::sync::Notify>,
    /// Where pictures may be fetched from. The bot's token is sent with
    /// every download, so only Slack itself is in the list.
    image_hosts: Vec<String>,
    /// The client that fetches pictures. It follows no redirects: the
    /// token is on the request, and a redirect could take it elsewhere.
    downloads: reqwest::Client,
}

impl SlackChannel {
    /// Create the adapter.
    ///
    /// `channels` lists the conversations that may talk to the assistant. An
    /// empty list means nobody can.
    pub fn new(token: impl Into<String>, channels: Vec<String>, users: Vec<String>) -> Self {
        SlackChannel {
            token: token.into(),
            channels,
            users: users.into_iter().collect(),
            client: reqwest::Client::new(),
            api_root: API.to_string(),
            poll_seconds: POLL_SECONDS,
            bot_id: tokio::sync::OnceCell::new(),
            image_hosts: vec!["https://slack.com".to_string()],
            downloads: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            seen: Seen::new(),
            shutdown: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Point the adapter at a different API root.
    pub fn with_api_root(mut self, root: impl Into<String>) -> Self {
        self.api_root = root.into().trim_end_matches('/').to_string();
        self
    }

    /// Change how often the conversations are polled.
    pub fn with_poll_seconds(mut self, seconds: u64) -> Self {
        self.poll_seconds = seconds.max(1);
        self
    }

    /// Read the token and the allow-lists from the environment.
    ///
    /// Returns `None` when no token is configured, so the caller can skip
    /// the channel rather than failing.
    pub fn from_env() -> Option<Self> {
        let token = crate::secrets::var("SLACK_BOT_TOKEN")?;
        let channels = split_list("SLACK_ALLOWED_CHANNELS");
        let users = split_list("SLACK_ALLOWED_USERS");
        Some(SlackChannel::new(token, channels, users))
    }

    /// The conversations this adapter speaks in.
    pub fn channels(&self) -> &[String] {
        &self.channels
    }

    /// Whether a user may talk to the assistant.
    pub fn is_user_allowed(&self, user: &str) -> bool {
        // An empty user list means every user in an allowed conversation may
        // talk. The conversation list is the outer gate.
        self.users.is_empty() || self.users.contains(user)
    }

    /// Fetch the pictures that came with a message.
    ///
    /// Slack keeps its files to itself: the address they are at only works
    /// with the bot's token. Only the first few are taken.
    async fn fetch_files(&self, files: &[SlackFile]) -> Vec<crate::image::Image> {
        let mut images = Vec::new();

        for file in files.iter().take(3) {
            let Some(url) = &file.url_private else {
                continue;
            };
            let Some(mimetype) = file.mimetype.as_deref() else {
                continue;
            };
            if !mimetype.starts_with("image/") {
                continue;
            }

            // The address comes with the message, so it is checked before
            // the bot's token is sent there: only Slack itself may see it.
            if !is_allowed_url(url, &self.image_hosts) {
                tracing::warn!("ignoring a file address that is not Slack's");
                continue;
            }

            let Ok(response) = self
                .downloads
                .get(url)
                .header("authorization", format!("Bearer {}", self.token))
                .send()
                .await
            else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(bytes) = response.bytes().await else {
                continue;
            };
            if let Ok(image) = crate::image::Image::from_bytes(&bytes, mimetype) {
                images.push(image);
            }
        }

        images
    }

    /// Fetch pictures from places other than Slack's own servers.
    ///
    /// The bot's token is sent with every download, so the default is
    /// Slack alone; anything else here is for an installation that hosts
    /// its own files, or for a test.
    pub fn with_image_hosts(mut self, hosts: Vec<String>) -> Self {
        self.image_hosts = hosts;
        self
    }

    /// The bot's own user id, looked up once.
    async fn bot_id(&self) -> Option<String> {
        self.bot_id
            .get_or_try_init(|| async {
                let response = self.get("users.info").await?;
                Ok::<String, anyhow::Error>(response.id)
            })
            .await
            .ok()
            .cloned()
    }

    /// Make a GET request against the API.
    async fn get(&self, path: &str) -> anyhow::Result<SlackUser> {
        let response = self
            .client
            .get(format!("{}/{path}", self.api_root))
            .header("authorization", format!("Bearer {}", self.token))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("slack {path} answered {status}: {body}");
        }

        let body: serde_json::Value = response.json().await?;
        if body.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let error = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("the request was refused");
            anyhow::bail!("slack {path} refused the request: {error}");
        }

        let user: SlackUser = serde_json::from_value(
            body.get("user")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("slack {path} answered without a user"))?,
        )?;
        Ok(user)
    }

    /// The messages of a conversation, newest first.
    async fn messages(&self, channel: &str) -> anyhow::Result<Vec<SlackMessage>> {
        let response = self
            .client
            .get(format!("{}/conversations.history", self.api_root))
            .header("authorization", format!("Bearer {}", self.token))
            .query(&[("channel", channel), ("limit", "50")])
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("slack conversations.history answered {status}: {body}");
        }

        let body: serde_json::Value = response.json().await?;
        if body.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let error = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("the request was refused");
            anyhow::bail!("slack refused to read the conversation: {error}");
        }

        let messages = body
            .get("messages")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("slack answered without any messages"))?;
        Ok(serde_json::from_value(messages)?)
    }

    /// Send a message to a conversation.
    pub async fn send_to_channel(&self, channel: &str, text: &str) -> anyhow::Result<()> {
        for chunk in split_message(text, MESSAGE_LIMIT) {
            let response = self
                .client
                .post(format!("{}/chat.postMessage", self.api_root))
                .header("authorization", format!("Bearer {}", self.token))
                .json(&json!({ "channel": channel, "text": chunk }))
                .send()
                .await?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("slack refused the message: {status}: {body}");
            }

            // Slack answers with 200 even when it says no.
            let body: serde_json::Value = response.json().await?;
            if body.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                let error = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("the request was refused");
                anyhow::bail!("slack refused the message: {error}");
            }
        }
        Ok(())
    }
}

/// Split a comma-separated environment variable into its parts.
fn split_list(name: &str) -> Vec<String> {
    crate::secrets::var(name)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Turn Slack's `seconds.microseconds` stamp into a number, so newness can
/// be compared.
fn message_number(ts: &str) -> Option<u64> {
    let (seconds, rest) = ts.split_once('.')?;
    let seconds: u64 = seconds.parse().ok()?;
    let micros: u64 = rest.get(..6)?.parse().ok()?;
    Some(seconds * 1_000_000 + micros)
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn run(
        &self,
        inbound_tx: InboundSender,
        _outbound: OutboundSender,
    ) -> anyhow::Result<()> {
        if self.channels.is_empty() {
            tracing::warn!(
                "slack channel has no allowed conversations — set SLACK_ALLOWED_CHANNELS to enable it"
            );
        }

        let bot = self.bot_id().await;

        loop {
            for channel in &self.channels {
                let messages = match self.messages(channel).await {
                    Ok(messages) => messages,
                    Err(e) => {
                        tracing::warn!("could not read the slack conversation {channel}: {e}");
                        continue;
                    }
                };

                // The first poll only marks where the conversation is now.
                let last = match self.seen.last(channel) {
                    Some(last) => last,
                    None => {
                        let newest = messages
                            .iter()
                            .filter_map(|m| message_number(&m.ts))
                            .max()
                            .unwrap_or(0);
                        self.seen.mark(channel, newest);
                        continue;
                    }
                };

                // The API gives the newest message first; the conversation
                // runs the other way.
                let mut fresh: Vec<&SlackMessage> = messages
                    .iter()
                    .filter(|m| {
                        message_number(&m.ts)
                            .map(|number| number > last)
                            .unwrap_or(false)
                    })
                    .collect();
                fresh.sort_by_key(|m| message_number(&m.ts).unwrap_or(0));

                for message in fresh {
                    if let Some(number) = message_number(&message.ts) {
                        self.seen.mark(channel, number);
                    }

                    // Never answer the assistant's own messages, or another
                    // bot's, and ignore the notices Slack mixes in.
                    if message.bot_id.is_some() {
                        continue;
                    }
                    if !message.kind.is_empty() && message.kind != "message" {
                        continue;
                    }
                    let author = match &message.user {
                        Some(user) => user,
                        None => continue,
                    };
                    if bot.as_deref() == Some(author.as_str()) {
                        continue;
                    }
                    if !self.is_user_allowed(author) {
                        continue;
                    }

                    let text = message.text.trim();

                    // A picture may come on its own, with no words.
                    let images = self.fetch_files(&message.files).await;
                    if text.is_empty() && images.is_empty() {
                        continue;
                    }

                    let inbound = InboundMessage {
                        sender: author.clone(),
                        sender_name: None,
                        text: text.to_string(),
                        conversation: channel.clone(),
                        channel: self.name().to_string(),
                        reply_to: None,
                        images,
                    };

                    if inbound_tx.send(inbound).await.is_err() {
                        return Ok(());
                    }
                }
            }

            tokio::select! {
                _ = self.shutdown.notified() => {
                    tracing::info!("slack channel shutting down");
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(self.poll_seconds)) => {}
            }
        }
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.shutdown.notify_one();
        Ok(())
    }

    async fn send(&self, message: &OutboundMessage) -> anyhow::Result<()> {
        self.send_to_channel(&message.conversation, &message.text)
            .await
    }

    fn is_authorized(&self, sender: &str) -> bool {
        self.is_user_allowed(sender)
    }
}
