//! Discord channel adapter.
//!
//! Uses the REST API and polls the channels it has been allowed to speak in.
//! Polling rather than the gateway websocket keeps the adapter to one HTTP
//! client and lets it work on a machine that cannot be reached from the
//! internet.
//!
//! The token comes from `DISCORD_BOT_TOKEN` and is never written to disk.
//! Only the channels listed in `DISCORD_ALLOWED_CHANNELS` are polled, and an
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

/// The Discord API root.
const API: &str = "https://discord.com/api/v10";

/// Discord refuses messages over 2000 characters.
const MESSAGE_LIMIT: usize = 1900;

/// How often the channels are polled, in seconds.
const POLL_SECONDS: u64 = 3;

/// A message on a Discord channel.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub author: Option<DiscordUser>,
    #[serde(default)]
    pub mentions: Vec<DiscordUser>,
    /// The files that came with the message.
    #[serde(default)]
    pub attachments: Vec<DiscordAttachment>,
}

/// A file attached to a Discord message.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordAttachment {
    /// Where the file can be fetched from.
    pub url: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

/// The author of a message.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
}

impl DiscordUser {
    fn display_name(&self) -> String {
        self.global_name
            .clone()
            .unwrap_or_else(|| self.username.clone())
    }
}

/// The Discord adapter.
#[derive(Clone)]
pub struct DiscordChannel {
    token: String,
    channels: Vec<String>,
    users: HashSet<String>,
    client: reqwest::Client,
    /// The API root, so a proxy or a test can stand in for Discord.
    api_root: String,
    /// How often the channels are polled.
    poll_seconds: u64,
    /// The bot's own user id, so it does not answer itself.
    bot_id: tokio::sync::OnceCell<String>,
    /// How far each channel has been read.
    seen: Seen,
    /// Set when the adapter should stop.
    shutdown: Arc<tokio::sync::Notify>,
    /// Where pictures may be fetched from.
    image_hosts: Vec<String>,
    /// The client that fetches pictures. It follows no redirects, so a
    /// picture cannot send the assistant somewhere else.
    downloads: reqwest::Client,
}

impl DiscordChannel {
    /// Create the adapter.
    ///
    /// `channels` lists the channel ids that may talk to the assistant. An
    /// empty list means nobody can.
    pub fn new(token: impl Into<String>, channels: Vec<String>, users: Vec<String>) -> Self {
        DiscordChannel {
            token: token.into(),
            channels,
            users: users.into_iter().collect(),
            client: reqwest::Client::new(),
            api_root: API.to_string(),
            poll_seconds: POLL_SECONDS,
            bot_id: tokio::sync::OnceCell::new(),
            image_hosts: vec![
                "https://cdn.discordapp.com".to_string(),
                "https://discordapp.net".to_string(),
            ],
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

    /// Change how often the channels are polled.
    pub fn with_poll_seconds(mut self, seconds: u64) -> Self {
        self.poll_seconds = seconds.max(1);
        self
    }

    /// Read the token and the allow-lists from the environment.
    ///
    /// Returns `None` when no token is configured, so the caller can skip the
    /// channel rather than failing.
    pub fn from_env() -> Option<Self> {
        let token = crate::secrets::var("DISCORD_BOT_TOKEN")?;
        let channels = split_list("DISCORD_ALLOWED_CHANNELS");
        let users = split_list("DISCORD_ALLOWED_USERS");
        Some(DiscordChannel::new(token, channels, users))
    }

    /// The channels this adapter speaks in.
    pub fn channels(&self) -> &[String] {
        &self.channels
    }

    /// The bot's own user id, looked up once.
    async fn bot_id(&self) -> Option<&str> {
        self.bot_id
            .get_or_try_init(|| async {
                let user: DiscordUser = self
                    .get("users/@me")
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok::<String, anyhow::Error>(user.id)
            })
            .await
            .ok()
            .map(|id| id.as_str())
    }

    /// Make a GET request against the API.
    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> anyhow::Result<T> {
        let response = self
            .client
            .get(format!("{}/{path}", self.api_root))
            .header("authorization", format!("Bot {}", self.token))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("discord {path} answered {status}: {body}");
        }

        Ok(response.json().await?)
    }

    /// The messages of a channel, newest first.
    async fn messages(&self, channel: &str) -> anyhow::Result<Vec<DiscordMessage>> {
        self.get(&format!("channels/{channel}/messages?limit=50"))
            .await
    }

    /// Send a message to a channel.
    pub async fn send_to_channel(&self, channel: &str, text: &str) -> anyhow::Result<()> {
        for chunk in split_message(text, MESSAGE_LIMIT) {
            let response = self
                .client
                .post(format!("{}/channels/{channel}/messages", self.api_root))
                .header("authorization", format!("Bot {}", self.token))
                .json(&json!({ "content": chunk }))
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("discord refused the message: {status}: {body}");
            }
        }
        Ok(())
    }

    /// Fetch the pictures that came with a message.
    ///
    /// Discord serves its attachments from a public address, so they are
    /// fetched as they are. Only the first few are taken: a message with
    /// twenty pictures in it is not something the model needs to see.
    async fn fetch_attachments(
        &self,
        attachments: &[DiscordAttachment],
    ) -> Vec<crate::image::Image> {
        let mut images = Vec::new();

        for attachment in attachments.iter().take(3) {
            let Some(content_type) = attachment.content_type.as_deref() else {
                continue;
            };
            if !content_type.starts_with("image/") {
                continue;
            }

            // Only Discord's own servers are fetched from, so a message
            // cannot point the assistant at a machine of its own.
            if !is_allowed_url(&attachment.url, &self.image_hosts) {
                tracing::warn!("ignoring an attachment address that is not Discord's");
                continue;
            }

            let Ok(response) = self.downloads.get(&attachment.url).send().await else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(bytes) = response.bytes().await else {
                continue;
            };
            if let Ok(image) = crate::image::Image::from_bytes(&bytes, content_type) {
                images.push(image);
            }
        }

        images
    }

    /// Fetch pictures from places other than Discord's own servers.
    ///
    /// The default is Discord alone, so a message cannot point the
    /// assistant at a machine of its own; anything else here is for an
    /// installation that hosts its own files, or for a test.
    pub fn with_image_hosts(mut self, hosts: Vec<String>) -> Self {
        self.image_hosts = hosts;
        self
    }

    /// Whether a user may talk to the assistant.
    pub fn is_user_allowed(&self, user: &str) -> bool {
        // An empty user list means every user in an allowed channel may
        // talk. The channel list is the outer gate.
        self.users.is_empty() || self.users.contains(user)
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

/// Turn a Discord message id into a number, so newness can be compared.
fn message_number(id: &str) -> Option<u64> {
    id.parse().ok()
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn run(
        &self,
        inbound_tx: InboundSender,
        _outbound: OutboundSender,
    ) -> anyhow::Result<()> {
        if self.channels.is_empty() {
            tracing::warn!(
                "discord channel has no allowed channels — set DISCORD_ALLOWED_CHANNELS to enable it"
            );
        }

        let bot = self.bot_id().await.map(|id| id.to_string());

        loop {
            for channel in &self.channels {
                let messages = match self.messages(channel).await {
                    Ok(messages) => messages,
                    Err(e) => {
                        tracing::warn!("could not read the discord channel {channel}: {e}");
                        continue;
                    }
                };

                // The first poll only marks where the conversation is now, so
                // the assistant does not answer a backlog of old messages.
                let last = match self.seen.last(channel) {
                    Some(last) => last,
                    None => {
                        let newest = messages
                            .iter()
                            .filter_map(|m| message_number(&m.id))
                            .max()
                            .unwrap_or(0);
                        self.seen.mark(channel, newest);
                        continue;
                    }
                };

                // The API gives the newest message first; the conversation
                // runs the other way.
                let mut fresh: Vec<&DiscordMessage> = messages
                    .iter()
                    .filter(|m| {
                        message_number(&m.id)
                            .map(|number| number > last)
                            .unwrap_or(false)
                    })
                    .collect();
                fresh.sort_by_key(|m| message_number(&m.id).unwrap_or(0));

                for message in fresh {
                    if let Some(number) = message_number(&message.id) {
                        self.seen.mark(channel, number);
                    }

                    // Never answer the assistant's own messages.
                    let author = match &message.author {
                        Some(author) => author,
                        None => continue,
                    };
                    if bot.as_deref() == Some(author.id.as_str()) {
                        continue;
                    }
                    if !self.is_user_allowed(&author.id) {
                        continue;
                    }

                    let text = message.content.trim();

                    // A picture may come on its own, with no words.
                    let images = self.fetch_attachments(&message.attachments).await;
                    if text.is_empty() && images.is_empty() {
                        continue;
                    }

                    let inbound = InboundMessage {
                        sender: author.id.clone(),
                        sender_name: Some(author.display_name()),
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
                    tracing::info!("discord channel shutting down");
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
