//! WhatsApp channel adapter.
//!
//! Uses the WhatsApp Cloud API: messages are sent through the Graph API, and
//! arriving messages are delivered by Meta to a webhook. Unlike the other
//! adapters there is nothing to poll — WhatsApp only pushes.
//!
//! The credentials come from the environment, or from the file `lia setup`
//! writes:
//!
//! - `WHATSAPP_TOKEN` — the access token
//! - `WHATSAPP_PHONE_NUMBER_ID` — the id of the sending number
//! - `WHATSAPP_VERIFY_TOKEN` — the token Meta echoes when the webhook is set up
//! - `WHATSAPP_APP_SECRET` — the app secret used to sign webhook deliveries
//! - `WHATSAPP_ALLOWED_NUMBERS` — who may talk to the assistant
//!
//! The webhook listens on `127.0.0.1:8788` at `/webhooks/whatsapp` by
//! default; `WHATSAPP_WEBHOOK_HOST` and `WHATSAPP_WEBHOOK_PORT` move it.
//! Meta must be able to reach that address over HTTPS, so it is usually put
//! behind a tunnel.
//!
//! Only numbers on the allow-list may talk to the assistant, and an empty
//! list means nobody can. A delivery is checked against the app secret when
//! one is configured; without it the webhook still works, but anyone who
//! knows the address could post a message, so the secret is strongly
//! recommended.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use tokio::sync::{Notify, OnceCell};

use super::{
    Channel, InboundMessage, InboundSender, OutboundMessage, OutboundSender, split_message,
};
use crate::secrets;

/// The Graph API root.
const API: &str = "https://graph.facebook.com/v22.0";

/// WhatsApp refuses a text body over 4096 characters.
const MESSAGE_LIMIT: usize = 4000;

/// Where Meta delivers messages.
pub const WEBHOOK_PATH: &str = "/webhooks/whatsapp";

/// The default address the webhook is bound to.
const DEFAULT_WEBHOOK_HOST: &str = "127.0.0.1";
const DEFAULT_WEBHOOK_PORT: u16 = 8788;

/// A WhatsApp Cloud API channel.
#[derive(Clone)]
pub struct WhatsAppChannel {
    token: String,
    phone_number_id: String,
    verify_token: String,
    app_secret: Option<String>,
    allowed: HashSet<String>,
    /// The API root, so a proxy or a test can stand in for Meta.
    api_root: String,
    webhook_host: String,
    webhook_port: u16,
    client: reqwest::Client,
    /// Set when the adapter should stop.
    shutdown: Arc<Notify>,
    /// The address the webhook was actually bound to, once `run` starts.
    bound: Arc<OnceCell<SocketAddr>>,
}

impl WhatsAppChannel {
    /// Create the adapter.
    ///
    /// `allowed` lists the phone numbers that may talk to the assistant. An
    /// empty list means nobody can.
    pub fn new(
        token: impl Into<String>,
        phone_number_id: impl Into<String>,
        verify_token: impl Into<String>,
        allowed: Vec<String>,
    ) -> Self {
        WhatsAppChannel {
            token: token.into(),
            phone_number_id: phone_number_id.into(),
            verify_token: verify_token.into(),
            app_secret: None,
            allowed: allowed
                .into_iter()
                .map(|number| clean_number(&number))
                .filter(|number| !number.is_empty())
                .collect(),
            api_root: API.to_string(),
            webhook_host: DEFAULT_WEBHOOK_HOST.to_string(),
            webhook_port: DEFAULT_WEBHOOK_PORT,
            client: reqwest::Client::new(),
            shutdown: Arc::new(Notify::new()),
            bound: Arc::new(OnceCell::new()),
        }
    }

    /// Read the credentials and the allow-list from the environment, or the
    /// secrets file.
    ///
    /// Returns `None` when the essentials are missing, so the caller can skip
    /// the channel rather than failing.
    pub fn from_env() -> Option<Self> {
        let token = secrets::var("WHATSAPP_TOKEN")?;
        let phone_number_id = secrets::var("WHATSAPP_PHONE_NUMBER_ID")?;
        let verify_token = secrets::var("WHATSAPP_VERIFY_TOKEN")?;
        let app_secret = secrets::var("WHATSAPP_APP_SECRET");
        if app_secret.is_none() {
            tracing::warn!(
                "WHATSAPP_APP_SECRET is not set; webhook deliveries will not be verified"
            );
        }
        let allowed = secrets::var("WHATSAPP_ALLOWED_NUMBERS")
            .unwrap_or_default()
            .split(',')
            .map(|number| number.trim().to_string())
            .filter(|number| !number.is_empty())
            .collect();

        let mut channel = WhatsAppChannel::new(token, phone_number_id, verify_token, allowed);
        if let Some(secret) = app_secret {
            channel = channel.with_app_secret(secret);
        }
        if let Some(root) = secrets::var("WHATSAPP_API_ROOT") {
            channel = channel.with_api_root(root);
        }
        let host = secrets::var("WHATSAPP_WEBHOOK_HOST")
            .unwrap_or_else(|| DEFAULT_WEBHOOK_HOST.to_string());
        let port = secrets::var("WHATSAPP_WEBHOOK_PORT")
            .and_then(|port| port.trim().parse::<u16>().ok())
            .unwrap_or(DEFAULT_WEBHOOK_PORT);
        Some(channel.with_webhook(host, port))
    }

    /// Point the adapter at a different API root.
    pub fn with_api_root(mut self, root: impl Into<String>) -> Self {
        self.api_root = root.into().trim_end_matches('/').to_string();
        self
    }

    /// Use this app secret to check webhook signatures.
    pub fn with_app_secret(mut self, secret: impl Into<String>) -> Self {
        self.app_secret = Some(secret.into());
        self
    }

    /// Listen for webhooks on this address.
    pub fn with_webhook(mut self, host: impl Into<String>, port: u16) -> Self {
        self.webhook_host = host.into();
        self.webhook_port = port;
        self
    }

    /// The address the webhook is bound to, after [`Channel::run`] starts.
    pub fn webhook_address(&self) -> Option<SocketAddr> {
        self.bound.get().copied()
    }

    /// Whether a number may talk to the assistant.
    pub fn is_number_allowed(&self, number: &str) -> bool {
        self.allowed.contains(&clean_number(number))
    }

    /// The numbers that may talk to the assistant.
    pub fn allowed_numbers(&self) -> impl Iterator<Item = &str> {
        self.allowed.iter().map(String::as_str)
    }

    fn messages_url(&self) -> String {
        format!("{}/{}/messages", self.api_root, self.phone_number_id.trim())
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    fn is_authorized(&self, sender: &str) -> bool {
        self.is_number_allowed(sender)
    }

    async fn run(&self, inbound: InboundSender, _outbound: OutboundSender) -> Result<()> {
        let state = WebhookState {
            inbound,
            verify_token: self.verify_token.clone(),
            app_secret: self.app_secret.clone(),
            allowed: self.allowed.clone(),
            warned: Arc::new(AtomicBool::new(false)),
        };

        let app = Router::new()
            .route(WEBHOOK_PATH, get(verify).post(receive))
            .with_state(state);

        let listener =
            tokio::net::TcpListener::bind((self.webhook_host.as_str(), self.webhook_port))
                .await
                .with_context(|| {
                    format!(
                        "could not listen for WhatsApp webhooks on {}:{}",
                        self.webhook_host, self.webhook_port
                    )
                })?;
        let address = listener.local_addr()?;
        let _ = self.bound.set(address);
        tracing::info!("whatsapp webhook listening on http://{address}{WEBHOOK_PATH}");

        let shutdown = self.shutdown.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown.notified().await;
            })
            .await
            .context("the WhatsApp webhook server stopped")?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        // `notify_one` keeps the signal when no task is waiting yet, so a
        // shutdown before `run` is not lost.
        self.shutdown.notify_one();
        Ok(())
    }

    async fn send(&self, message: &OutboundMessage) -> Result<()> {
        if message.text.trim().is_empty() {
            return Ok(());
        }

        let to = clean_number(&message.conversation);
        if to.is_empty() {
            anyhow::bail!("the WhatsApp conversation has no number");
        }

        for chunk in split_message(&message.text, MESSAGE_LIMIT) {
            let body = json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": to,
                "type": "text",
                "text": {
                    "body": chunk,
                    "preview_url": false,
                },
            });

            let response = self
                .client
                .post(self.messages_url())
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
                .context("could not reach the WhatsApp API")?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                anyhow::bail!("WhatsApp refused the message ({status}): {text}");
            }
        }
        Ok(())
    }
}

/// What the webhook handlers share.
#[derive(Clone)]
struct WebhookState {
    inbound: InboundSender,
    verify_token: String,
    app_secret: Option<String>,
    allowed: HashSet<String>,
    /// Whether the missing-secret warning has been printed.
    warned: Arc<AtomicBool>,
}

/// The query Meta sends when the webhook is registered.
#[derive(Debug, Deserialize)]
struct Verification {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    token: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
}

/// Answer Meta's subscription check.
async fn verify(State(state): State<WebhookState>, Query(query): Query<Verification>) -> Response {
    let subscribed = query.mode.as_deref() == Some("subscribe");
    let token_matches = query.token.as_deref() == Some(state.verify_token.as_str());
    match (subscribed && token_matches, query.challenge) {
        (true, Some(challenge)) => (StatusCode::OK, challenge).into_response(),
        _ => (StatusCode::FORBIDDEN, "verification failed").into_response(),
    }
}

/// Receive a delivery from Meta.
async fn receive(State(state): State<WebhookState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(secret) = &state.app_secret {
        let signature = headers
            .get("x-hub-signature-256")
            .and_then(|value| value.to_str().ok());
        if !signature_matches(secret, &body[..], signature) {
            tracing::warn!("a WhatsApp delivery had no valid signature");
            return (StatusCode::FORBIDDEN, "bad signature").into_response();
        }
    } else if !state.warned.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "WHATSAPP_APP_SECRET is not set; the webhook cannot tell a real delivery from a forged one"
        );
    }

    let messages = match messages_from(&body) {
        Ok(messages) => messages,
        Err(e) => {
            tracing::warn!("a WhatsApp delivery could not be read: {e}");
            return (StatusCode::BAD_REQUEST, "bad payload").into_response();
        }
    };

    for message in messages {
        if !state.allowed.contains(&clean_number(&message.sender)) {
            tracing::debug!(
                "a WhatsApp message from {} was not on the allow-list",
                message.sender
            );
            continue;
        }
        if state.inbound.send(message).await.is_err() {
            break;
        }
    }

    // Meta retries anything that is not a 200, so delivery is acknowledged
    // as soon as the message is on the queue.
    (StatusCode::OK, "ok").into_response()
}

/// Check the `X-Hub-Signature-256` header against the app secret.
pub fn signature_matches(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    let Some(header) = header else {
        return false;
    };
    let Some(encoded) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(encoded) else {
        return false;
    };

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    let actual = mac.finalize().into_bytes();

    // Compare in constant time, so the header cannot be guessed a byte at a
    // time.
    if expected.len() != actual.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in expected.iter().zip(actual.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

/// Read the messages out of a webhook delivery.
///
/// Only text messages become [`InboundMessage`]s: status updates, receipts
/// and media are left for another day.
pub fn messages_from(body: &[u8]) -> Result<Vec<InboundMessage>> {
    let delivery: Delivery =
        serde_json::from_slice(body).context("the delivery is not the JSON WhatsApp sends")?;

    let mut messages = Vec::new();
    for entry in delivery.entry {
        for change in entry.changes {
            if !change.field.is_empty() && change.field != "messages" {
                continue;
            }

            let names: HashMap<String, Option<String>> = change
                .value
                .contacts
                .iter()
                .map(|contact| {
                    (
                        clean_number(&contact.wa_id),
                        contact
                            .profile
                            .as_ref()
                            .and_then(|profile| profile.name.clone()),
                    )
                })
                .collect();

            for message in change.value.messages {
                if !message.kind.is_empty() && message.kind != "text" {
                    continue;
                }
                let Some(text) = message.text else {
                    continue;
                };
                let text = text.body.trim();
                if text.is_empty() {
                    continue;
                }

                let sender = clean_number(&message.from);
                if sender.is_empty() {
                    continue;
                }

                let mut inbound = InboundMessage::new(
                    "whatsapp",
                    sender.clone(),
                    text.to_string(),
                    sender.clone(),
                );
                inbound.sender_name = names.get(&sender).cloned().flatten();
                messages.push(inbound);
            }
        }
    }

    Ok(messages)
}

/// Remove the punctuation people put in a phone number.
///
/// The Cloud API wants digits, and the allow-list should match whether the
/// number was written with a leading `+` or spaces.
pub fn clean_number(number: &str) -> String {
    number
        .trim()
        .chars()
        .filter(|character| !matches!(character, '+' | ' ' | '-' | '(' | ')' | '.' | '_'))
        .collect()
}

/// The shape of a webhook delivery, as far as it matters here.
#[derive(Debug, Deserialize)]
struct Delivery {
    #[serde(default)]
    entry: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default)]
    changes: Vec<Change>,
}

#[derive(Debug, Deserialize)]
struct Change {
    #[serde(default)]
    field: String,
    #[serde(default)]
    value: Value,
}

#[derive(Debug, Default, Deserialize)]
struct Value {
    #[serde(default)]
    contacts: Vec<Contact>,
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
struct Contact {
    #[serde(default)]
    wa_id: String,
    #[serde(default)]
    profile: Option<Profile>,
}

#[derive(Debug, Deserialize)]
struct Profile {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    from: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<Text>,
}

#[derive(Debug, Deserialize)]
struct Text {
    #[serde(default)]
    body: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_is_compared_without_its_punctuation() {
        assert_eq!(clean_number("+1 (555) 123-4567"), "15551234567");
        assert!(clean_number("15551234567") == clean_number("+1 555-123-4567"));
    }

    #[test]
    fn a_signature_is_checked() {
        let body = br#"{"hello":"there"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").expect("a key");
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(signature_matches("secret", body, Some(&signature)));
        assert!(!signature_matches("another", body, Some(&signature)));
        assert!(!signature_matches("secret", body, None));
        assert!(!signature_matches("secret", body, Some("sha256=zzzz")));
    }

    #[test]
    fn a_text_message_becomes_an_inbound_message() {
        let body = br#"{
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "1",
                "changes": [{
                    "field": "messages",
                    "value": {
                        "messaging_product": "whatsapp",
                        "contacts": [{
                            "wa_id": "15551234567",
                            "profile": { "name": "Alice" }
                        }],
                        "messages": [{
                            "from": "15551234567",
                            "id": "wamid.1",
                            "timestamp": "1700000000",
                            "type": "text",
                            "text": { "body": "hello there" }
                        }]
                    }
                }]
            }]
        }"#;

        let messages = messages_from(body).expect("the delivery is read");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "hello there");
        assert_eq!(messages[0].sender, "15551234567");
        assert_eq!(messages[0].sender_name.as_deref(), Some("Alice"));
        assert_eq!(messages[0].conversation, "15551234567");
        assert_eq!(messages[0].channel, "whatsapp");
    }

    #[test]
    fn a_status_is_not_a_message() {
        let body = br#"{
            "entry": [{
                "changes": [{
                    "field": "messages",
                    "value": {
                        "statuses": [{ "id": "wamid.1", "status": "read" }]
                    }
                }]
            }]
        }"#;
        let messages = messages_from(body).expect("the delivery is read");
        assert!(messages.is_empty(), "a status became a message");
    }

    #[test]
    fn only_the_allowed_numbers_are_accepted() {
        let channel = WhatsAppChannel::new("token", "1", "verify", vec!["+1 555 123 4567".into()]);
        assert!(channel.is_number_allowed("15551234567"));
        assert!(!channel.is_number_allowed("15550000000"));

        let empty = WhatsAppChannel::new("token", "1", "verify", Vec::new());
        assert!(!empty.is_number_allowed("15551234567"));
    }
}
