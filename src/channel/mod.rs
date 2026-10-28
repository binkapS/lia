//! Channel adapters: messaging platforms connected to the assistant.
//!
//! A channel turns a platform's messages into [`InboundMessage`]s and sends
//! [`OutboundMessage`]s back. The agent loop is unaware of which platform a
//! message came from, so adding a platform means implementing [`Channel`] and
//! nothing else.
//!
//! Inbound messages are treated as untrusted: a channel that can be reached
//! by anyone restricts who may talk to the assistant, and tools that could
//! change the system are gated behind an approval policy.

pub mod discord;
pub mod pairing;
pub mod slack;
pub mod telegram;
pub mod terminal;
pub mod whatsapp;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::agent::{AgentEvent, SessionRunner};
use crate::provider::types::{ContentBlock, Message};
use crate::tool::approval::{ApprovalDecision, ApprovalRequest};

pub use pairing::{Grant, PairingStore, Request};

/// A message arriving from a platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Platform-specific sender id.
    pub sender: String,
    /// Display name of the sender, when the platform provides one.
    pub sender_name: Option<String>,
    /// The message body.
    pub text: String,
    /// A conversation or thread id, used to group messages into a session.
    pub conversation: String,
    /// Which platform this came from.
    pub channel: String,
    /// Set when this message continues an existing conversation.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The pictures that came with the message.
    #[serde(default)]
    pub images: Vec<crate::image::Image>,
}

impl InboundMessage {
    pub fn new(
        channel: impl Into<String>,
        sender: impl Into<String>,
        text: impl Into<String>,
        conversation: impl Into<String>,
    ) -> Self {
        InboundMessage {
            sender: sender.into(),
            sender_name: None,
            text: text.into(),
            conversation: conversation.into(),
            channel: channel.into(),
            reply_to: None,
            images: Vec::new(),
        }
    }

    /// The same message, with pictures.
    pub fn with_images(mut self, images: Vec<crate::image::Image>) -> Self {
        self.images = images;
        self
    }
}

/// A message going out to a platform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Where to send it.
    pub conversation: String,
    /// The body.
    pub text: String,
    /// Whether this is a reply to a specific message.
    pub reply_to: Option<String>,
}

impl OutboundMessage {
    pub fn new(conversation: impl Into<String>, text: impl Into<String>) -> Self {
        OutboundMessage {
            conversation: conversation.into(),
            text: text.into(),
            reply_to: None,
        }
    }
}

/// Where a channel sends the replies it produces.
pub type OutboundSender = mpsc::Sender<OutboundMessage>;

/// Where a channel delivers the messages it receives.
pub type InboundSender = mpsc::Sender<InboundMessage>;

/// A messaging platform adapter.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Stable name for the platform, e.g. `telegram`.
    fn name(&self) -> &str;

    /// Run the adapter.
    ///
    /// `inbound` receives messages addressed to the assistant; `outbound`
    /// carries the replies back to the platform.
    async fn run(&self, inbound: InboundSender, outbound: OutboundSender) -> anyhow::Result<()>;

    /// Stop the adapter.
    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Send a message out through the platform.
    ///
    /// The conversation in the message is the platform's own id for it — a
    /// chat id, a channel id, a room id. Adapters that cannot send do
    /// nothing, rather than failing the whole reply loop.
    async fn send(&self, message: &OutboundMessage) -> anyhow::Result<()> {
        let _ = message;
        Ok(())
    }

    /// Whether a sender is allowed to talk to the assistant.
    ///
    /// Channels that can be reached by anyone use this to require pairing.
    fn is_authorized(&self, sender: &str) -> bool {
        let _ = sender;
        true
    }
}

/// How a channel routes a conversation to a session id.
pub fn session_id_for(channel: &str, conversation: &str) -> String {
    format!("{channel}:{conversation}")
}

/// How far each conversation has been read.
///
/// A channel that polls has to know where it left off, and it has to know it
/// for every conversation it watches. The first poll of a conversation only
/// marks where the conversation is: the assistant does not answer a backlog
/// of messages that were sent before it came up.
#[derive(Clone, Default)]
pub struct Seen {
    inner: Arc<std::sync::Mutex<HashMap<String, u64>>>,
}

impl Seen {
    /// A fresh tracker.
    pub fn new() -> Self {
        Seen::default()
    }

    /// The id of the last message read in a conversation, if it has been
    /// read at all.
    pub fn last(&self, conversation: &str) -> Option<u64> {
        let inner = self.inner.lock().unwrap();
        inner.get(conversation).copied()
    }

    /// Mark a message as read.
    pub fn mark(&self, conversation: &str, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.entry(conversation.to_string()).or_insert(0);
        // The ids only grow, so a late message cannot move the mark back.
        if id > *entry {
            *entry = id;
        }
    }
}

/// A command a user can send instead of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Start a fresh conversation.
    Reset,
    /// Say what the assistant can do.
    Help,
    /// Let a tool run.
    Approve {
        /// Which request to answer; `None` means the only one waiting.
        id: Option<String>,
        /// Whether the answer covers the rest of the session.
        always: bool,
    },
    /// Refuse a tool.
    Deny {
        /// Which request to answer; `None` means the only one waiting.
        id: Option<String>,
    },
    /// Let someone in who has asked to be let in.
    Pair {
        /// Which code to approve.
        code: Option<String>,
    },
}

/// Parse a control command, if the message is one.
///
/// The command may carry the bot's name after an `@`, as Telegram appends
/// it when the bot is mentioned in a group.
pub fn parse_control(text: &str) -> Option<Control> {
    let mut words = text.split_whitespace();
    let first = words.next()?;
    if !first.starts_with('/') {
        return None;
    }
    let name = first
        .trim_start_matches('/')
        .split('@')
        .next()
        .unwrap_or("");

    // The rest of the line is the argument: which request, and whether the
    // answer covers the rest of the session.
    let rest: Vec<&str> = words.collect();
    let id = rest
        .iter()
        .find(|word| !word.starts_with('/'))
        .map(|word| word.to_string());
    let always = rest.contains(&"always");

    match name {
        "reset" | "new" => Some(Control::Reset),
        "help" | "start" => Some(Control::Help),
        "approve" | "yes" | "allow" => Some(Control::Approve { id, always }),
        "deny" | "no" => Some(Control::Deny { id }),
        "pair" => Some(Control::Pair { code: id }),
        _ => None,
    }
}

/// The answer to a help request on a channel with no text of its own.
const GENERIC_HELP: &str = "I'm lia, a local AI assistant. Send me a message and I'll work on it.";

/// Whether an address is one of the allowed ones.
///
/// The address of a picture arrives with the message, so it is checked
/// before anything is fetched from it: the bot's own token is sent along
/// with the request, and it must not go to a place the message chose.
///
/// An entry is a scheme and a host, e.g. `https://slack.com`. A host
/// matches itself and the names below it, so `files.slack.com` is covered
/// by `slack.com`.
pub(crate) fn is_allowed_url(url: &str, allowed: &[String]) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };

    allowed.iter().any(|entry| {
        let Ok(base) = reqwest::Url::parse(entry) else {
            return false;
        };
        if base.scheme() != parsed.scheme() {
            return false;
        }
        let Some(base_host) = base.host_str() else {
            return false;
        };
        host == base_host || host.ends_with(&format!(".{base_host}"))
    })
}

/// Split a long message into chunks a platform will accept.
///
/// Chat platforms cap the length of a message, and a reply that is too long
/// is refused rather than truncated, so it is split at line boundaries where
/// possible. A line that is longer than the limit on its own is cut at the
/// nearest character boundary, so no chunk is ever over the limit.
///
/// The chunks join back to the original text.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return vec![text.to_string()];
    }
    if text.len() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in text.split_inclusive('\n') {
        // A line that cannot fit in a chunk of its own is cut up.
        if line.len() > limit {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let mut rest = line;
            while rest.len() > limit {
                let mut cut = limit;
                while cut > 0 && !rest.is_char_boundary(cut) {
                    cut -= 1;
                }
                chunks.push(rest[..cut].to_string());
                rest = &rest[cut..];
            }
            current.push_str(rest);
            continue;
        }

        if current.len() + line.len() > limit && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// The permission requests of the conversations, shared by everything that
/// might ask in one.
///
/// A conversation has one approver, whoever asks in it: the channels ask
/// when the assistant wants to run something, and a scheduled job asks in
/// the chat that scheduled it. If each had an approver of its own, an
/// answer in the chat would go to the wrong one.
#[derive(Clone, Default)]
pub struct Approvers {
    inner: Arc<tokio::sync::Mutex<HashMap<String, Arc<ChatApprover>>>>,
}

impl Approvers {
    /// A registry with nothing in it yet.
    pub fn new() -> Approvers {
        Approvers::default()
    }

    /// The approver of a conversation, made once.
    pub async fn get(
        &self,
        channel: Arc<dyn Channel>,
        conversation: &str,
        timeout: std::time::Duration,
    ) -> Arc<ChatApprover> {
        let key = session_id_for(channel.name(), conversation);
        let mut approvers = self.inner.lock().await;
        approvers
            .entry(key)
            .or_insert_with(|| {
                Arc::new(ChatApprover::new(channel, conversation).with_timeout(timeout))
            })
            .clone()
    }
}

/// Connects a channel to the agent.
///
/// Inbound messages are answered by the agent; the reply goes back out
/// through the same channel. The dispatcher also holds the permission
/// requests of each conversation, so an answer to one can be matched to it.
#[derive(Clone)]
pub struct ChannelDispatcher {
    runner: Arc<SessionRunner>,
    events: mpsc::Sender<AgentEvent>,
    /// The channel this dispatcher serves.
    channel: Arc<dyn Channel>,
    /// The approver of each conversation.
    approvers: Approvers,
    /// The lock that keeps one turn at a time in a conversation.
    turns: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// How long a chat waits for an answer to a permission request.
    approval_timeout: std::time::Duration,
}

impl ChannelDispatcher {
    pub fn new(
        runner: Arc<SessionRunner>,
        events: mpsc::Sender<AgentEvent>,
        channel: Arc<dyn Channel>,
    ) -> Self {
        ChannelDispatcher::with_approvers(runner, events, channel, Approvers::new())
    }

    /// A dispatcher that shares its permission requests with others.
    pub fn with_approvers(
        runner: Arc<SessionRunner>,
        events: mpsc::Sender<AgentEvent>,
        channel: Arc<dyn Channel>,
        approvers: Approvers,
    ) -> Self {
        ChannelDispatcher {
            runner,
            events,
            channel,
            approvers,
            turns: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            approval_timeout: APPROVAL_TIMEOUT,
        }
    }

    /// Wait this long for an answer to a permission request.
    pub fn with_approval_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }

    /// The approver of a conversation, made once.
    async fn approver_for(&self, conversation: &str) -> Arc<ChatApprover> {
        self.approvers
            .get(self.channel.clone(), conversation, self.approval_timeout)
            .await
    }

    /// Answer a command that is not a message for the model.
    ///
    /// Returns `None` when the message is not a command, so the caller can
    /// pass it on to the assistant.
    pub async fn handle_command(&self, inbound: &InboundMessage) -> Option<String> {
        let session_id = session_id_for(&inbound.channel, &inbound.conversation);

        match parse_control(&inbound.text) {
            Some(Control::Reset) => {
                self.runner.reset_session(&session_id).await;
                Some("Started a new conversation.".to_string())
            }
            Some(Control::Help) => Some(GENERIC_HELP.to_string()),
            Some(Control::Approve { id, always }) => {
                let decision = if always {
                    ApprovalDecision::ApproveAlways
                } else {
                    ApprovalDecision::Approve
                };
                self.answer(inbound, id.as_deref(), decision).await
            }
            Some(Control::Deny { id }) => {
                self.answer(inbound, id.as_deref(), ApprovalDecision::Deny)
                    .await
            }
            Some(Control::Pair { code }) => {
                // Only the owner may approve a code, and the owner is at
                // the terminal or in a chat that is already allowed.
                Some(match code {
                    Some(code) => {
                        format!("Ask the owner to approve {code}: `lia pair approve {code}`.")
                    }
                    None => {
                        "Ask the owner to approve the code: `lia pair approve <code>`.".to_string()
                    }
                })
            }
            None => None,
        }
    }

    /// Answer a permission request.
    async fn answer(
        &self,
        inbound: &InboundMessage,
        id: Option<&str>,
        decision: ApprovalDecision,
    ) -> Option<String> {
        let approver = self.approver_for(&inbound.conversation).await;
        if approver.resolve(id, &inbound.sender, decision) {
            Some("Done.".to_string())
        } else {
            Some("There is no tool waiting for that answer.".to_string())
        }
    }

    /// Handle one inbound message, returning the assistant's reply.
    pub async fn handle(&self, inbound: InboundMessage) -> Option<String> {
        let session_id = session_id_for(&inbound.channel, &inbound.conversation);

        // A command is answered by the assistant itself and never reaches
        // the model.
        if let Some(reply) = self.handle_command(&inbound).await {
            return Some(reply);
        }

        // One turn at a time in a conversation, so two messages cannot
        // write over each other's history.
        let turn_lock = {
            let mut turns = self.turns.lock().await;
            turns
                .entry(inbound.conversation.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = turn_lock.lock().await;

        // The chat is asked when a tool needs permission.
        let approver = self.approver_for(&inbound.conversation).await;
        approver.set_sender(&inbound.sender);
        self.runner
            .set_approver(
                &session_id,
                Arc::clone(&approver) as Arc<dyn crate::agent::ApprovalHandler>,
            )
            .await;

        let (tx, mut rx) = mpsc::channel(256);

        let runner = self.runner.clone();
        let content = inbound.text.clone();
        let images: Vec<ContentBlock> = inbound
            .images
            .iter()
            .map(|image| ContentBlock::Image {
                media_type: image.media_type.clone(),
                data: image.data.clone(),
            })
            .collect();
        let message = if images.is_empty() {
            Message::user(content)
        } else {
            Message::user_with_images(content, images)
        };
        let id = session_id.clone();
        let turn = tokio::spawn(async move {
            runner.run_message(&id, Some(message), tx).await;
        });

        // Relay the turn's events to the channel's event bus, and build the
        // reply text from the streamed output.
        let mut reply = String::new();
        let mut notes: Vec<String> = Vec::new();
        while let Some(event) = rx.recv().await {
            match &event {
                AgentEvent::TextDelta(text) => reply.push_str(text),
                AgentEvent::ToolCallStarted { name, .. } => {
                    notes.push(format!("🔧 {name}"));
                }
                AgentEvent::Error(message) => {
                    notes.push(format!("⚠ {message}"));
                }
                _ => {}
            }
            let _ = self.events.send(event).await;
        }

        let _ = turn.await;

        if !notes.is_empty() {
            reply = format!("{}\n\n{}", notes.join("\n"), reply);
        }
        Some(reply)
    }
}

/// The channels the environment configures.
///
/// A channel that has no credentials is skipped rather than being an error:
/// running the assistant with only Telegram set up should still work.
pub fn channels_from_env(pairing: Option<Arc<PairingStore>>) -> Vec<Arc<dyn Channel>> {
    let mut channels: Vec<Arc<dyn Channel>> = Vec::new();

    if let Some(telegram) = telegram::TelegramChannel::from_env() {
        let telegram = match pairing.clone() {
            Some(store) => telegram.with_pairing(store),
            None => telegram,
        };
        channels.push(Arc::new(telegram));
    }
    if let Some(discord) = discord::DiscordChannel::from_env() {
        channels.push(Arc::new(discord));
    }
    if let Some(slack) = slack::SlackChannel::from_env() {
        channels.push(Arc::new(slack));
    }
    if let Some(whatsapp) = whatsapp::WhatsAppChannel::from_env() {
        channels.push(Arc::new(whatsapp));
    }

    channels
}

/// Run a set of channels against the agent.
///
/// Each channel gets its own queue. Inbound messages are answered by the
/// agent, and the replies go back out through the same channel.
///
/// The permission requests of the conversations are shared with the rest
/// of the process, so a question asked by a scheduled job is answered in
/// the same place as one asked by the chat itself.
pub fn spawn_channels(
    channels: Vec<Arc<dyn Channel>>,
    runner: Arc<SessionRunner>,
    events: mpsc::Sender<AgentEvent>,
    approvers: Approvers,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    for channel in channels {
        let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundMessage>(64);
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<OutboundMessage>(64);

        // The adapter runs on its own task and pushes the messages it
        // receives into the queue.
        let adapter = channel.clone();
        let adapter_outbound = outbound_tx.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = adapter.run(inbound_tx, adapter_outbound).await {
                tracing::error!("the {} channel failed: {e}", adapter.name());
            }
        }));

        // Inbound messages are answered by the agent. Each channel has its
        // own dispatcher, because each has its own conversations.
        let dispatcher = Arc::new(ChannelDispatcher::with_approvers(
            runner.clone(),
            events.clone(),
            channel.clone(),
            approvers.clone(),
        ));
        handles.push(tokio::spawn(async move {
            while let Some(message) = inbound_rx.recv().await {
                let conversation = message.conversation.clone();

                // A command is answered at once, even while a turn is
                // running: a turn may be waiting for exactly this answer.
                if let Some(reply) = dispatcher.handle_command(&message).await {
                    let _ = outbound_tx
                        .send(OutboundMessage::new(conversation, reply))
                        .await;
                    continue;
                }

                // The turn runs in the background, so the next message is
                // read while the assistant works on this one.
                let dispatcher = dispatcher.clone();
                let outbound_tx = outbound_tx.clone();
                tokio::spawn(async move {
                    let reply = dispatcher.handle(message).await;
                    let Some(reply) = reply else { return };
                    let _ = outbound_tx
                        .send(OutboundMessage::new(conversation, reply))
                        .await;
                });
            }
        }));

        // The replies go back out through the platform.
        let sender = channel.clone();
        handles.push(tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                if let Err(e) = sender.send(&message).await {
                    tracing::warn!("could not send to {}: {e}", sender.name());
                }
            }
        }));
    }

    handles
}

/// An approver for places where the platform cannot ask.
///
/// A chat message is not a place to ask "are you sure you want to run this?" —
/// the answer would be a message too, and it might come from someone else.
/// So the channel is treated as non-interactive and the configured
/// `on_no_interaction` policy decides.
pub struct ChannelApprover;

#[async_trait]
impl crate::agent::ApprovalHandler for ChannelApprover {
    async fn request(&self, _request: ApprovalRequest) -> Option<ApprovalDecision> {
        Some(ApprovalDecision::Deny)
    }

    fn is_interactive(&self) -> bool {
        false
    }
}

/// How long a chat waits for an answer to a permission request.
pub const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// An approver that asks in the chat and waits for the answer.
///
/// The question is posted to the conversation, and the person who asked the
/// assistant something answers it with `/approve <id>` or `/deny <id>`. The
/// id is in the message, so the answer reaches the right request, and only
/// the person the assistant is working for may answer it.
///
/// If nobody answers in time, the request is refused: a tool that nobody
/// agreed to does not run.
pub struct ChatApprover {
    /// The channel to ask in.
    channel: Arc<dyn Channel>,
    /// The conversation to ask in.
    conversation: String,
    /// The requests waiting for an answer.
    pending: Arc<std::sync::Mutex<HashMap<String, Pending>>>,
    /// The next id to hand out.
    next_id: Arc<std::sync::atomic::AtomicU64>,
    /// Who asked the assistant something; only they may answer.
    sender: Arc<std::sync::Mutex<Option<String>>>,
    /// How long to wait for an answer.
    timeout: std::time::Duration,
}

/// A permission request waiting for an answer.
struct Pending {
    /// Who may answer it.
    sender: String,
    /// Where the answer goes.
    answer: tokio::sync::oneshot::Sender<ApprovalDecision>,
}

impl ChatApprover {
    /// An approver that asks in a conversation.
    pub fn new(channel: Arc<dyn Channel>, conversation: impl Into<String>) -> Self {
        ChatApprover {
            channel,
            conversation: conversation.into(),
            pending: Arc::new(std::sync::Mutex::new(HashMap::new())),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            sender: Arc::new(std::sync::Mutex::new(None)),
            timeout: APPROVAL_TIMEOUT,
        }
    }

    /// Wait this long for an answer before refusing.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The conversation this approver asks in.
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// Note who the assistant is working for.
    pub fn set_sender(&self, sender: &str) {
        let mut current = self.sender.lock().unwrap();
        *current = Some(sender.to_string());
    }

    /// Answer a request, if there is one and the answer comes from the
    /// person who may give it.
    pub fn resolve(&self, id: Option<&str>, sender: &str, decision: ApprovalDecision) -> bool {
        let mut pending = self.pending.lock().unwrap();

        // With no id given, the answer goes to the only request waiting.
        let id = match id {
            Some(id) => id.to_string(),
            None => {
                if pending.len() != 1 {
                    return false;
                }
                pending.keys().next().cloned().unwrap_or_default()
            }
        };

        let Some(entry) = pending.get(&id) else {
            return false;
        };
        // Only the person the assistant is working for may answer.
        if !entry.sender.is_empty() && entry.sender != sender {
            return false;
        }

        let entry = pending.remove(&id).expect("the request was just there");
        entry.answer.send(decision).is_ok()
    }
}

#[async_trait]
impl crate::agent::ApprovalHandler for ChatApprover {
    async fn request(&self, request: ApprovalRequest) -> Option<ApprovalDecision> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string();
        let (answer, waiting) = tokio::sync::oneshot::channel();

        let sender = self.sender.lock().unwrap().clone().unwrap_or_default();
        self.pending
            .lock()
            .unwrap()
            .insert(id.clone(), Pending { sender, answer });

        let text = format!(
            "The assistant wants to run `{tool}`.\n```\n{arguments}\n```\n\
             Reply `/approve {id}` to let it, or `/deny {id}` to refuse.",
            tool = request.tool,
            arguments = request.arguments,
            id = id,
        );

        if let Err(e) = self
            .channel
            .send(&OutboundMessage::new(self.conversation.clone(), text))
            .await
        {
            tracing::warn!("could not ask for permission in the chat: {e}");
            self.pending.lock().unwrap().remove(&id);
            return None;
        }

        // Nobody answering in time is a refusal.
        match tokio::time::timeout(self.timeout, waiting).await {
            Ok(Ok(decision)) => Some(decision),
            _ => {
                self.pending.lock().unwrap().remove(&id);
                None
            }
        }
    }

    fn is_interactive(&self) -> bool {
        true
    }
}
