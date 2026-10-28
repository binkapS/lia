//! Telegram channel adapter.
//!
//! Uses the Bot API over long polling. The bot token comes from
//! `TELEGRAM_BOT_TOKEN`; the token itself is never written to disk.
//!
//! Only chats listed in `allowed_chats` may talk to the assistant, and an
//! empty allow-list means nobody can — an open bot would let anyone on the
//! internet run commands on this machine.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::pairing::{self, PairingStore};
use super::{
    Channel, Control, InboundMessage, InboundSender, OutboundMessage, OutboundSender,
    parse_control, split_message,
};

/// The Telegram API root.
const API: &str = "https://api.telegram.org";

/// Long-poll timeout in seconds.
const POLL_TIMEOUT: u64 = 30;

/// A chat the bot is allowed to respond in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AllowedChat {
    /// A numeric chat id, which for groups is negative.
    Id(i64),
    /// A username, without the leading `@`.
    Username(String),
}

impl AllowedChat {
    /// Parse a chat id or username.
    pub fn parse(value: &str) -> Self {
        let trimmed = value.trim().trim_start_matches('@');
        match trimmed.parse::<i64>() {
            Ok(id) => AllowedChat::Id(id),
            Err(_) => AllowedChat::Username(trimmed.to_lowercase()),
        }
    }

    /// Whether this entry covers the given chat.
    pub fn matches(&self, chat: &TelegramChat) -> bool {
        match self {
            AllowedChat::Id(id) => chat.id == *id,
            AllowedChat::Username(name) => chat
                .username
                .as_deref()
                .map(|u| u.eq_ignore_ascii_case(name))
                .unwrap_or(false),
        }
    }
}

/// A Telegram chat.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramChat {
    pub id: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

/// A Telegram message.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<TelegramUser>,
    pub chat: TelegramChat,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub reply_to_message: Option<Box<TelegramMessage>>,
    /// The pictures, smallest first. The last one is the biggest.
    #[serde(default)]
    pub photo: Option<Vec<TelegramPhoto>>,
}

/// A picture in a Telegram message.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramPhoto {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// The file behind a picture.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TelegramFile {
    #[serde(default)]
    pub file_path: Option<String>,
}

/// A Telegram user.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
}

impl TelegramUser {
    fn display_name(&self) -> String {
        match (&self.first_name, &self.last_name) {
            (Some(first), Some(last)) => format!("{first} {last}"),
            (Some(first), None) => first.clone(),
            (None, None) => self.username.clone().unwrap_or_else(|| self.id.to_string()),
            (None, Some(last)) => last.clone(),
        }
    }
}

/// An update from the Telegram API.
#[derive(Debug, Clone, Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
    #[serde(default)]
    edited_message: Option<TelegramMessage>,
}

/// The response wrapper the Telegram API returns.
#[derive(Debug, Clone, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

/// The Telegram bot adapter.
pub struct TelegramChannel {
    token: String,
    allowed: HashSet<AllowedChat>,
    client: reqwest::Client,
    /// Set when the adapter should stop.
    shutdown: tokio::sync::Notify,
    /// Where the people who have been let in are kept.
    pairing: Option<Arc<PairingStore>>,
}

impl TelegramChannel {
    /// Create the adapter.
    ///
    /// `allowed` lists the chats that may talk to the assistant. An empty list
    /// means nobody can.
    pub fn new(token: impl Into<String>, allowed: Vec<AllowedChat>) -> Self {
        TelegramChannel {
            token: token.into(),
            allowed: allowed.into_iter().collect(),
            client: reqwest::Client::new(),
            shutdown: tokio::sync::Notify::new(),
            pairing: None,
        }
    }

    /// Let people in who have asked to be let in.
    ///
    /// Without a pairing store, only the chats on the allow-list may talk
    /// to the assistant.
    pub fn with_pairing(mut self, store: Arc<PairingStore>) -> Self {
        self.pairing = Some(store);
        self
    }

    /// Read the token and allow-list from the environment.
    ///
    /// Returns `None` when no token is configured, so the caller can skip the
    /// channel rather than failing.
    pub fn from_env() -> Option<Self> {
        let token = crate::secrets::var("TELEGRAM_BOT_TOKEN")?;
        let allowed = crate::secrets::var("TELEGRAM_ALLOWED_CHATS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(AllowedChat::parse)
            .collect();
        Some(TelegramChannel::new(token, allowed))
    }

    /// Whether a chat is allowed to talk to the assistant.
    pub fn is_chat_allowed(&self, chat: &TelegramChat) -> bool {
        self.allowed.iter().any(|a| a.matches(chat))
    }

    /// Whether someone may talk to the assistant in this chat.
    ///
    /// A chat on the allow-list may. So may a person who was let in with a
    /// pairing code — but only in a private chat, so a group cannot be
    /// let in as a whole.
    pub fn may_talk(&self, chat: &TelegramChat, sender: &str) -> bool {
        if self.is_chat_allowed(chat) {
            return true;
        }

        let Some(store) = &self.pairing else {
            return false;
        };
        if chat.kind.as_deref() != Some("private") {
            return false;
        }
        store.is_granted(self.name(), &chat.id.to_string(), sender)
    }

    /// Answer a request to let someone in.
    ///
    /// Only a chat that is already on the allow-list may approve a code.
    /// Otherwise anyone could let themselves in.
    async fn handle_pair(&self, chat: &TelegramChat, code: Option<&str>) -> String {
        let Some(store) = &self.pairing else {
            return "Pairing is not set up here.".to_string();
        };

        if !self.is_chat_allowed(chat) {
            return "Only someone I already talk to can let others in.".to_string();
        }

        let Some(code) = code else {
            let waiting = store.requests();
            if waiting.is_empty() {
                return "Nobody is waiting to be let in.".to_string();
            }
            let lines: Vec<String> = waiting.iter().map(|request| request.describe()).collect();
            return format!("Give a code to approve. Waiting:\n{}", lines.join("\n"));
        };

        match store.approve(code) {
            Ok(grant) => format!("Let in: {}", grant.describe()),
            Err(e) => format!("Not let in: {e}."),
        }
    }

    async fn call<T>(&self, method: &str, body: &Value) -> anyhow::Result<Option<T>>
    where
        T: for<'de> Deserialize<'de> + Default,
    {
        let url = format!("{API}/bot{}/{}", self.token, method);
        let response = self.client.post(&url).json(body).send().await?;
        let parsed: ApiResponse<T> = response.json().await?;
        if !parsed.ok {
            let description = parsed
                .description
                .unwrap_or_else(|| "unknown error".to_string());
            tracing::warn!("telegram {method} failed: {description}");
            return Ok(None);
        }
        Ok(parsed.result)
    }

    /// Fetch a picture the platform sent.
    ///
    /// Telegram hands out a file id rather than the picture itself, so it
    /// is asked where the file is and then fetched. The biggest size of a
    /// picture is used: the smaller ones are the same picture, smaller.
    async fn fetch_photo(&self, photo: &TelegramPhoto) -> Option<crate::image::Image> {
        let file = self
            .call::<TelegramFile>("getFile", &json!({ "file_id": photo.file_id }))
            .await
            .ok()??;
        let path = file.file_path?;
        if photo.file_size.unwrap_or(0) > crate::image::MAX_BYTES as u64 {
            tracing::warn!("the picture on telegram is too big to send to the model");
            return None;
        }

        let url = format!("{API}/file/bot{}/{path}", self.token);
        let bytes = self.client.get(url).send().await.ok()?.bytes().await.ok()?;
        let media_type =
            crate::image::media_type_of(std::path::Path::new(&path)).unwrap_or("image/jpeg");
        crate::image::Image::from_bytes(&bytes, media_type).ok()
    }

    /// Send a message to a chat.
    pub async fn send_to_chat(&self, chat_id: i64, text: &str) -> anyhow::Result<()> {
        // Telegram rejects messages over 4096 characters.
        for chunk in split_message(text, 4000) {
            self.call::<Value>(
                "sendMessage",
                &json!({
                    "chat_id": chat_id,
                    "text": chunk,
                    "disable_web_page_preview": true,
                }),
            )
            .await?;
        }
        Ok(())
    }
}

/// The answer to a help request on Telegram.
const HELP_TEXT: &str = "I'm lia, a local AI assistant. Just send me a message and I'll work on it.

/reset — start a fresh conversation
/help — show this message

When I need permission to run something, I ask here. Answer with
/approve <id> or /deny <id>.";

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn run(&self, inbound_tx: InboundSender, outbound: OutboundSender) -> anyhow::Result<()> {
        if self.allowed.is_empty() {
            tracing::warn!(
                "telegram channel has no allowed chats — set TELEGRAM_ALLOWED_CHATS to enable it"
            );
        }

        let mut offset: Option<i64> = None;

        loop {
            // Long-poll for updates.
            let mut params = vec![
                ("timeout", POLL_TIMEOUT.to_string()),
                ("allowed_updates", "[\"message\"]".to_string()),
            ];
            if let Some(o) = offset {
                params.push(("offset", o.to_string()));
            }

            let url = format!("{API}/bot{}/getUpdates", self.token);
            let response = self.client.get(&url).query(&params).send().await;

            let updates = match response {
                Ok(r) => match r.json::<ApiResponse<Vec<Update>>>().await {
                    Ok(parsed) if parsed.ok => parsed.result.unwrap_or_default(),
                    Ok(parsed) => {
                        tracing::warn!(
                            "telegram getUpdates failed: {}",
                            parsed.description.unwrap_or_default()
                        );
                        Vec::new()
                    }
                    Err(e) => {
                        tracing::warn!("could not read telegram response: {e}");
                        Vec::new()
                    }
                },
                Err(e) => {
                    tracing::warn!("could not reach telegram: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            for update in updates {
                offset = Some(update.update_id + 1);

                let message = match update.message.or(update.edited_message) {
                    Some(m) => m,
                    None => continue,
                };

                let text = message
                    .text
                    .clone()
                    .or_else(|| message.caption.clone())
                    .unwrap_or_default();

                // A picture may come on its own, with no words.
                let images = match message.photo.as_ref().and_then(|photos| photos.last()) {
                    Some(photo) => self.fetch_photo(photo).await.into_iter().collect(),
                    None => Vec::new(),
                };
                if text.trim().is_empty() && images.is_empty() {
                    continue;
                }

                let sender_id = message
                    .from
                    .as_ref()
                    .map(|u| u.id.to_string())
                    .unwrap_or_else(|| message.chat.id.to_string());
                let sender_name = message.from.as_ref().map(|u| u.display_name());

                // Only chats on the allow-list may talk to the assistant.
                if !self.may_talk(&message.chat, &sender_id) {
                    // A stranger in a private chat may be let in later, so
                    // they are given a code to give the owner.
                    if let Some(store) = &self.pairing
                        && message.chat.kind.as_deref() == Some("private")
                    {
                        let (request, offer) = store.request(
                            self.name(),
                            &message.chat.id.to_string(),
                            &sender_id,
                            sender_name.clone(),
                        );
                        if offer {
                            let _ = self
                                .send_to_chat(
                                    message.chat.id,
                                    &pairing::pairing_message(&request.code),
                                )
                                .await;
                        }
                    }
                    continue;
                }

                // Handle control commands before handing off to the agent.
                // A reset is answered by the dispatcher, which is what owns
                // the session, so it is passed on rather than handled here.
                match parse_control(&text) {
                    Some(Control::Help) => {
                        let _ = self.send_to_chat(message.chat.id, HELP_TEXT).await;
                        continue;
                    }
                    Some(Control::Pair { code }) => {
                        let reply = self.handle_pair(&message.chat, code.as_deref()).await;
                        let _ = self.send_to_chat(message.chat.id, &reply).await;
                        continue;
                    }
                    _ => {}
                }

                let inbound = InboundMessage {
                    sender: sender_id,
                    sender_name,
                    text,
                    conversation: message.chat.id.to_string(),
                    channel: self.name().to_string(),
                    reply_to: None,
                    images,
                };

                // The dispatcher answers this and pushes the reply back out.
                let _ = inbound_tx.send(inbound).await;
            }

            // Check for shutdown between polls.
            tokio::select! {
                _ = self.shutdown.notified() => {
                    tracing::info!("telegram channel shutting down");
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
            }
        }

        let _ = outbound;
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.shutdown.notify_one();
        Ok(())
    }

    async fn send(&self, message: &OutboundMessage) -> anyhow::Result<()> {
        let chat_id = message
            .conversation
            .parse::<i64>()
            .map_err(|_| anyhow::anyhow!("'{}' is not a telegram chat id", message.conversation))?;
        self.send_to_chat(chat_id, &message.text).await
    }

    fn is_authorized(&self, sender: &str) -> bool {
        // A private chat's id is the person's own id, so the allow-list
        // and the pairing grants are both read by that id.
        let allowed = self.allowed.iter().any(|chat| match chat {
            AllowedChat::Id(id) => id.to_string() == sender,
            AllowedChat::Username(_) => false,
        });
        allowed
            || self
                .pairing
                .as_ref()
                .is_some_and(|store| store.is_granted(self.name(), sender, sender))
    }
}
