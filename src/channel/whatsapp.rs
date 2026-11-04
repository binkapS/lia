//! WhatsApp channel adapter.
//!
//! A linked device over WhatsApp Web, the way [OpenClaw](https://openclaw.ai)
//! links one: the gateway owns the session, pairs it by qr code, and reuses
//! it on every restart. There are no tokens, no webhook and nothing to
//! expose over HTTPS — the adapter opens the connection itself.
//!
//! The first run draws the qr code in this terminal: open WhatsApp on the
//! phone, go to *Linked devices*, and scan it. The session is kept in the
//! state directory, afterwards the adapter reconnects by itself. A drop
//! ends the session and a fresh one is dialled after a pause that grows
//! with the failures, so the server is not hammered while it fumbles.
//!
//! An unknown number that writes in a private chat is treated the way the
//! Telegram adapter treats an unknown chat: it is given a pairing code, and
//! the owner lets them in. Numbers on `WHATSAPP_ALLOWED_NUMBERS` skip that.
//!
//! While the assistant works on a reply, the chat is shown a typing
//! indicator and the message that started the turn is acknowledged with a
//! reaction, so the sender can see the message was received and is being
//! worked on: silence otherwise reads as "not delivered".

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use tokio::sync::{Notify, RwLock};

use whatsapp_rust::pair::NATIVE_CAMERA_DEEP_LINK_PREFIX;
use whatsapp_rust::prelude::*;

use super::pairing::{self, PairingStore};
use super::{
    Channel, InboundMessage, InboundSender, OutboundMessage, OutboundSender, split_message,
};
use crate::secrets;

/// A reply is split before it is sent, because WhatsApp refuses anything
/// over 4096 characters rather than truncating it. The limit is the same as
/// the Cloud API's, the way OpenClaw's chunking default is.
const MESSAGE_LIMIT: usize = 4000;

/// How often the typing indicator is refreshed: clients let a `composing`
/// presence expire after a few seconds.
const TYPING_REFRESH: Duration = Duration::from_secs(4);

/// How long a typing indicator may run before it is stopped on its own,
/// even if the reply is still working.
const TYPING_CAP: Duration = Duration::from_secs(15 * 60);

/// The pause after a drop before a fresh connection is dialled. The pauses
/// grow, so a server having its own bad day is not hammered.
const RECONNECT_PAUSES: [Duration; 5] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];

/// A session that lasted at least this long counts as a good connection:
/// the next drop starts the backstops over.
const GOOD_CONNECTION: Duration = Duration::from_secs(60);

/// The emoji a message is acknowledged with while the reply is worked on.
/// `WHATSAPP_ACK_REACTION` replaces it; an empty value turns it off.
const DEFAULT_ACK_REACTION: &str = "👀";

/// The client of the live session, if there is one.
type LiveClient = Arc<RwLock<Option<Arc<Client>>>>;

/// The typing-indicator task of each chat.
type SharedTyping = Arc<tokio::sync::Mutex<HashMap<String, tokio::task::AbortHandle>>>;

/// A WhatsApp channel linked through a qr code.
pub struct WhatsAppChannel {
    /// Where the linked session lives.
    store_path: PathBuf,
    /// The phone numbers and group ids that may talk to the assistant
    /// without pairing.
    allowed: HashSet<String>,
    /// Where the people who have been let in are kept.
    pairing: Option<Arc<PairingStore>>,
    /// Set when the adapter should stop. Shared, so the connection loop can
    /// be released from the outside.
    shutdown: Arc<Notify>,
    /// The client of the live session, kept fresh across reconnects.
    client: LiveClient,
    /// One typing refresher per chat.
    typing: SharedTyping,
    /// The reaction a message is acknowledged with, if any.
    ack_reaction: Option<String>,
    /// Whether accepted messages are marked as read.
    read_receipts: bool,
    /// The name the linked device shows as on the phone.
    push_name: Option<String>,
}

impl WhatsAppChannel {
    /// Create the adapter.
    ///
    /// `store_path` is the SQLite file of the linked session; a pairing by
    /// qr code is stored there and survives a restart. `allowed` lists the
    /// phone numbers (and group ids) that may talk without a pairing code;
    /// an empty list means pairing decides.
    pub fn new(store_path: PathBuf, allowed: Vec<String>) -> Self {
        WhatsAppChannel {
            store_path,
            allowed: allowed.iter().map(|n| clean_number(n)).collect(),
            pairing: None,
            shutdown: Arc::new(Notify::new()),
            client: Arc::new(RwLock::new(None)),
            typing: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            ack_reaction: Some(DEFAULT_ACK_REACTION.to_string()),
            read_receipts: true,
            push_name: None,
        }
    }

    /// Let people in who have asked to be let in.
    ///
    /// Without a pairing store, only the numbers on the allow-list may talk
    /// to the assistant.
    pub fn with_pairing(mut self, store: Arc<PairingStore>) -> Self {
        self.pairing = Some(store);
        self
    }

    /// Read the allow-list from the environment, or the secrets file.
    ///
    /// The state directory is where the linked session lives. Returns `None`
    /// when the state directory is unknown, so the channel is skipped rather
    /// than failing: WhatsApp asks for nothing but a scan.
    pub fn from_env(state_dir: &std::path::Path) -> Option<Self> {
        if state_dir.as_os_str().is_empty() {
            return None;
        }

        // The Cloud API's variables are retired; say so instead of letting
        // them silently do nothing.
        if secrets::var("WHATSAPP_TOKEN").is_some() {
            tracing::warn!(
                "WHATSAPP_TOKEN is not used any more: WhatsApp is linked by qr code, \
                 scan it the first time the channel comes up"
            );
        }

        let allowed = secrets::var("WHATSAPP_ALLOWED_NUMBERS")
            .unwrap_or_default()
            .split(',')
            .map(clean_number)
            .filter(|number| !number.is_empty())
            .collect();

        let mut channel = WhatsAppChannel::new(state_dir.join("whatsapp.db"), allowed);

        // An empty value turns the acknowledgement off; any other value
        // replaces the emoji.
        match secrets::var("WHATSAPP_ACK_REACTION") {
            Some(value) if value.trim().is_empty() => channel.ack_reaction = None,
            Some(value) if !value.trim().is_empty() => {
                channel.ack_reaction = Some(value.trim().to_string())
            }
            _ => {}
        }

        channel.read_receipts = match secrets::var("WHATSAPP_READ_RECEIPTS") {
            Some(value) => !matches!(value.trim(), "0" | "off" | "false" | "no"),
            None => true,
        };

        if let Some(name) = secrets::var("WHATSAPP_PUSH_NAME") {
            channel.push_name = Some(name);
        }

        Some(channel)
    }

    /// Whether a number or group may talk to the assistant.
    pub fn is_number_allowed(&self, number: &str) -> bool {
        self.allowed.contains(&clean_number(number))
    }

    /// The client of the live session, if there is one.
    async fn live(&self) -> Option<Arc<Client>> {
        self.client.read().await.clone()
    }
}

/// Show "typing…" in a chat, refreshed while the turn works.
///
/// The client of the live session is whatever the handler was handed: the
/// indicator takes the socket it runs on, not a lookup of the newest one.
async fn start_typing(typing: &SharedTyping, client: &Arc<Client>, chat: Jid) {
    let key = chat.to_string();
    let already = typing
        .lock()
        .await
        .get(&key)
        .filter(|handle| !handle.is_finished())
        .cloned();
    if already.is_some() {
        return;
    }

    let client = client.clone();
    let task = tokio::spawn(async move {
        let started = tokio::time::Instant::now();
        loop {
            // A broken indicator is not worth a warning: it is
            // best-effort feedback, not a receipt.
            let _ = client.chatstate().send_composing(&chat).await;
            if started.elapsed() >= TYPING_CAP {
                break;
            }
            tokio::time::sleep(TYPING_REFRESH).await;
        }
    });
    typing.lock().await.insert(key, task.abort_handle());
}

/// Stop showing "typing…" in a chat, before the reply replaces it.
async fn stop_typing(typing: &SharedTyping, conversation: &str) {
    if let Some(handle) = typing.lock().await.remove(conversation) {
        handle.abort();
    }
}

/// What the message handlers share; the connection hands messages over.
struct Inbox {
    inbound: InboundSender,
    allowed: HashSet<String>,
    pairing: Option<Arc<PairingStore>>,
    ack_reaction: Option<String>,
    read_receipts: bool,
    /// One typing refresher per chat, shared with the outbound side: the
    /// reply stops the indicator when it is on its way.
    typing: SharedTyping,
}

impl Inbox {
    /// The chat is answered by the assistant; an answer goes through the
    /// queue, so the whole connection is not held back by a long turn.
    async fn handle(&self, ctx: MessageContext) {
        // The linked account is the assistant's own: a message it sent
        // itself is not someone talking to it.
        if ctx.info.source.is_from_me {
            return;
        }

        let chat = ctx.info.source.chat.clone();

        // Status broadcasts and newsletters are read-only.
        if matches!(chat.server, Server::Broadcast | Server::Newsletter) {
            return;
        }

        let Some(text) = ctx.message.text_content() else {
            // Media and other enclosures are left for another day, as with
            // the Cloud API adapter before it.
            tracing::debug!("a whatsapp message without text was left alone");
            return;
        };
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        let sender = clean_number(&ctx.info.source.sender.to_string());
        let conversation = chat.to_string();

        if !self.may_talk(&chat, &sender) {
            // A stranger in a private chat may be let in later, so they are
            // given a code to give the owner.
            if let Some(store) = &self.pairing
                && chat.server != Server::Group
            {
                let (request, offer) = store.request("whatsapp", &conversation, &sender, None);
                if offer {
                    let offer = pairing::pairing_message(&request.code);
                    if let Err(e) = ctx.client.send_text(chat, offer).await {
                        tracing::warn!("could not offer a pairing code: {e}");
                    }
                }
            }
            return;
        }

        tracing::debug!("a whatsapp message came in from {sender}");

        // The receipt says the message was read; the reaction says the
        // assistant has it. Typing shows the turn is alive.
        if self.read_receipts {
            let id = ctx.info.id.as_str().to_string();
            if let Err(e) = ctx
                .client
                .mark_as_read(&chat, Some(&ctx.info.source.sender), &[id.as_str()])
                .await
            {
                tracing::debug!("could not mark a whatsapp message read: {e}");
            }
        }
        if let Some(ack) = &self.ack_reaction
            && let Err(e) = ctx.react(ack).await
        {
            tracing::debug!("could not acknowledge with a reaction: {e}");
        }

        let inbound = InboundMessage::new("whatsapp", sender, text.to_string(), conversation);
        // The sender sees "typing…" while the turn works; the reply stops
        // the indicator when it is on its way.
        start_typing(&self.typing, &ctx.client, chat).await;
        // A full queue holds the whole socket back; a message lost from the
        // queue is better than a connection gone silent.
        if self.inbound.try_send(inbound).is_err() {
            tracing::warn!("the whatsapp inbound queue is full; a message was dropped");
        }
    }

    /// Whether a sender may talk to the assistant in this chat.
    fn may_talk(&self, chat: &Jid, sender: &str) -> bool {
        if self.allowed.contains(&clean_number(&chat.to_string())) {
            return true;
        }

        let Some(store) = &self.pairing else {
            return false;
        };
        // Pairing is for private chats; a group is never let in as a whole.
        if chat.server == Server::Group {
            return false;
        }
        store.is_granted("whatsapp", &chat.to_string(), sender)
    }
}

/// Draw the pairing qr code in the terminal.
///
/// The payload is given WhatsApp's own deep-link prefix, so an iPhone's
/// camera opens WhatsApp straight to the linking screen; the phone's own
/// scanner takes the same bytes either way.
fn draw_qr(code: &str, timeout: Duration) {
    println!();
    println!("Link WhatsApp — WhatsApp → Settings → Linked devices → Link a device");
    println!("The code below is valid for {} seconds:", timeout.as_secs());
    let payload = format!("{NATIVE_CAMERA_DEEP_LINK_PREFIX}{code}");
    match qrcode::QrCode::new(payload.as_bytes()) {
        Ok(qr) => {
            let art = qr
                .render::<qrcode::render::unicode::Dense1x2>()
                .dark_color(qrcode::render::unicode::Dense1x2::Dark)
                .light_color(qrcode::render::unicode::Dense1x2::Light)
                .quiet_zone(true)
                .build();
            println!("{art}");
        }
        Err(e) => {
            tracing::warn!("the qr code could not be drawn: {e}");
            println!("The pairing payload is: {code}");
        }
    }
    println!();
}

/// How the adapter runs: one session at a time, as long as it lasts.
///
/// A drop ends the session and a fresh one is dialled after a pause that
/// grows with the failures. A pairing is never lost: it lives in the store,
/// and the next session picks it up.
async fn supervise(
    inbox: Arc<Inbox>,
    store_path: PathBuf,
    push_name: Option<String>,
    shutdown: Arc<Notify>,
    live: LiveClient,
) -> Result<()> {
    let mut failures = 0usize;

    loop {
        let store = SqliteStore::new(
            store_path
                .to_str()
                .context("the whatsapp session path is not text")?,
        )
        .await
        .context("the whatsapp session store could not be opened")?;

        let mut builder = Bot::builder()
            .with_backend(store)
            // Nothing is taken from the past: only messages that arrive from
            // now on are answered.
            .skip_history_sync();
        if let Some(name) = &push_name {
            builder = builder.with_push_name(name.clone());
        }

        let message_inbox = inbox.clone();
        let bot = builder
            .on_qr_code(|code, timeout| async move {
                draw_qr(&code, timeout);
            })
            .on_message(move |ctx| {
                let inbox = message_inbox.clone();
                async move {
                    inbox.handle(ctx).await;
                }
            })
            .on_connected(|_client| async move {
                tracing::info!("whatsapp linked and listening");
            })
            .on_logged_out(|_info| async move {
                tracing::warn!("whatsapp logged out: scan a fresh qr code to link again");
            })
            .build()
            .await
            .context("the whatsapp session could not be built")?;

        // The client is shared with `send`, which has no other way to reach
        // the socket.
        let client = bot.client();
        *live.write().await = Some(client.clone());

        let handle = bot.spawn();

        // The session runs until the phone unlinks, the connection is lost
        // or the gateway asks to stop. The client is disconnected by hand in
        // the second branch: dropping the handle alone leaves the socket up.
        let started = tokio::time::Instant::now();
        tokio::select! {
            _ = handle => {}
            _ = shutdown.notified() => {
                tracing::info!("whatsapp channel shutting down");
                client.disconnect().await;
                break;
            }
        }

        *live.write().await = None;
        if started.elapsed() >= GOOD_CONNECTION {
            failures = 0;
        }
        failures = failures + 1;
        let pause = RECONNECT_PAUSES[(failures - 1).min(RECONNECT_PAUSES.len() - 1)];
        tracing::warn!(
            "the whatsapp session ended; a fresh one is dialled in {}s",
            pause.as_secs()
        );

        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = shutdown.notified() => {
                tracing::info!("whatsapp channel shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Remove the punctuation people put in a phone number.
///
/// The allow-list should match whether the number was written with a
/// leading `+` or spaces.
pub fn clean_number(number: &str) -> String {
    number
        .trim()
        .chars()
        .filter(|character| !matches!(character, '+' | ' ' | '-' | '(' | ')' | '.' | '_' | '–'))
        .collect()
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    fn is_authorized(&self, sender: &str) -> bool {
        // A private chat's own address is the person's phone number, so the
        // allow-list and the pairing grants are both read by that address.
        let allowed = self.allowed.contains(&clean_number(sender));
        allowed
            || self
                .pairing
                .as_ref()
                .is_some_and(|store| store.is_granted(self.name(), sender, sender))
    }

    async fn run(&self, inbound: InboundSender, _outbound: OutboundSender) -> Result<()> {
        if self.allowed.is_empty() {
            tracing::warn!(
                "whatsapp channel has no allowed numbers — pair someone with a code, or set \
                 WHATSAPP_ALLOWED_NUMBERS"
            );
        }

        let inbox = Arc::new(Inbox {
            inbound,
            allowed: self.allowed.clone(),
            pairing: self.pairing.clone(),
            ack_reaction: self.ack_reaction.clone(),
            read_receipts: self.read_receipts,
            typing: self.typing.clone(),
        });

        supervise(
            inbox,
            self.store_path.clone(),
            self.push_name.clone(),
            self.shutdown.clone(),
            self.client.clone(),
        )
        .await
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

        // The indicator has said its piece; the reply is on its way.
        stop_typing(&self.typing, &message.conversation).await;

        let Some(client) = self.live().await else {
            anyhow::bail!(
                "whatsapp is not linked yet — scan the qr code from `lia serve`, then ask again"
            );
        };

        let to: Jid = message
            .conversation
            .parse()
            .with_context(|| format!("'{}' is not a whatsapp chat", message.conversation))?;

        for chunk in split_message(&message.text, MESSAGE_LIMIT) {
            // Three tries: a dying connection is sometimes revived by the
            // next dial rather than by waiting.
            let mut last_error = None;
            let mut sent = false;
            for attempt in 0..3 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_millis(800 * attempt as u64)).await;
                }
                match client.send_text(to.clone(), &chunk).await {
                    Ok(sent_message) => {
                        tracing::debug!(
                            "whatsapp accepted message {} for {to}",
                            sent_message.message_id
                        );
                        sent = true;
                        break;
                    }
                    Err(e) => last_error = Some(e),
                }
            }
            if !sent {
                let error = last_error.expect("an attempt was made");
                anyhow::bail!("whatsapp refused the message for {to}: {error}");
            }
        }
        Ok(())
    }
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
    fn without_an_allow_list_nobody_is_let_in() {
        let state = std::env::temp_dir().join("lia-test-whatsapp-nobody");
        let channel = WhatsAppChannel::new(state.join("w.db"), Vec::new());
        assert!(!channel.is_number_allowed("15551234567"));
    }

    #[test]
    fn a_number_on_the_allow_list_is_let_in() {
        let state = std::env::temp_dir().join("lia-test-whatsapp-known");
        let channel = WhatsAppChannel::new(state.join("w.db"), vec!["+1 555 123 4567".to_string()]);
        assert!(channel.is_number_allowed("15551234567"));
        assert!(!channel.is_number_allowed("15550000000"));
    }

    #[test]
    fn an_message_is_routed_to_the_right_chat() {
        let inbound = InboundMessage::new("whatsapp", "15551234567", "hello", "15551234567");
        assert_eq!(inbound.channel, "whatsapp");
        assert_eq!(inbound.conversation, "15551234567");
    }
}
