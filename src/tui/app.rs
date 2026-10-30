//! TUI application state and event loop.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::{mpsc, oneshot};

use crate::agent::session::SessionStore;
use crate::agent::{AgentEvent, SessionRunner};
use crate::config::{Config, LiveShare};
use crate::tool::approval::{ApprovalDecision, ApprovalRequest};
use crate::voice::{Voice, VoiceCommand, VoiceEvent, VoiceWorker};

use super::commands;
use super::input::Input;
use super::render;
use super::theme::Theme;
use super::wrap;
use crate::voice::compose::SpeechComposer;

/// A line in the transcript.
#[derive(Debug, Clone)]
pub enum Line {
    /// Text the assistant produced.
    Assistant(String),
    /// What the model was thinking, before it answered.
    Thinking(String),
    /// Text the user typed.
    User(String),
    /// A tool call, with its result once finished.
    Tool {
        id: String,
        name: String,
        input: String,
        result: Option<ToolResultView>,
    },
    /// What another assistant is doing, in the conversation that handed it
    /// its work.
    Sub {
        /// A few words saying what the work is.
        label: String,
        /// What it is doing now.
        note: String,
    },
    /// An informational note.
    Notice(String),
    /// An error.
    Error(String),
    /// A blank spacer.
    Blank,
}

/// The visible outcome of a tool call.
#[derive(Debug, Clone)]
pub struct ToolResultView {
    pub content: String,
    pub is_error: bool,
}

/// A pending approval request.
#[derive(Debug)]
pub struct PendingApproval {
    pub request: ApprovalRequest,
    /// Where the answer goes. Dropping it denies the call, so a request that
    /// is never answered cannot leave the agent waiting forever.
    responder: Option<oneshot::Sender<ApprovalDecision>>,
}

impl PendingApproval {
    fn new(request: ApprovalRequest) -> (Self, oneshot::Receiver<ApprovalDecision>) {
        let (responder, answer) = oneshot::channel();
        (
            PendingApproval {
                request,
                responder: Some(responder),
            },
            answer,
        )
    }

    /// Send the user's answer back to the waiting agent.
    fn answer(&mut self, decision: ApprovalDecision) {
        if let Some(responder) = self.responder.take() {
            let _ = responder.send(decision);
        }
    }
}

impl PartialEq for PendingApproval {
    fn eq(&self, other: &Self) -> bool {
        self.request == other.request
    }
}

/// Which popup is showing.
#[derive(Debug, PartialEq)]
pub enum Popup {
    None,
    /// A tool is waiting for approval.
    Approval(PendingApproval),
    /// The session list.
    Sessions,
    /// The help overlay.
    Help,
}

/// What the voice is doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum VoiceState {
    /// Nothing is being heard or said.
    #[default]
    Idle,
    /// The microphone is open.
    Listening {
        /// How loud it is, 0..=1.
        level: f32,
        /// How long the recording has been going, in seconds.
        elapsed: f64,
    },
    /// The words are being said.
    Speaking,
}

/// The application state.
pub struct App {
    pub config: Config,
    pub transcript: Vec<Line>,
    /// The text in the input box.
    pub input: Input,
    /// How wide the input box was last drawn, in cells. The key handler
    /// needs it to know which row of the wrapped text the cursor is on.
    pub input_width: u16,
    /// The index of the first visible row of the transcript.
    ///
    /// The view is anchored to a row rather than to the bottom, so text
    /// that arrives at the end does not drag the view along.
    pub scroll: usize,
    /// Whether the view follows the newest output.
    ///
    /// This is what the person asked for: it is turned off when they
    /// scroll back, and on again when they scroll to the bottom.
    pub follow: bool,
    /// The rows of the transcript that do not fit in the panel.
    ///
    /// The renderer measures it and writes it back, so the scroll
    /// mutators know how far there is to go.
    pub scroll_max: usize,
    /// How tall the transcript panel is, so a page is a known number of
    /// rows.
    pub view_rows: usize,
    /// Previously submitted inputs.
    pub history: Vec<String>,
    /// Index into the history while browsing.
    pub history_index: Option<usize>,
    /// The text that was being written when the history was first reached,
    /// so it can be put back when the browsing ends.
    pub history_draft: Option<(String, usize)>,
    /// The popup currently shown.
    pub popup: Popup,
    /// Whether the agent is working.
    pub busy: bool,
    /// The assistant's text as it streams in.
    pub streaming: String,
    /// The model's thinking as it streams in.
    pub thinking: String,
    /// The tool calls of the current turn.
    pub current_tools: Vec<Line>,
    /// Status line text.
    pub status: String,
    /// When the action now being shown began, for the activity animation.
    pub action_started: Option<Instant>,
    /// Speech waiting to be said, in the order it arrived.
    speech_queue: VecDeque<String>,
    /// Whether the speaker has one chunk and is working on it.
    speech_in_flight: bool,
    /// The part of the answer that has not reached a sentence boundary yet.
    speech_buffer: String,
    /// What the voice says and the state it keeps while saying it.
    composer: SpeechComposer,
    /// Which suggestion in the slash-command preview is highlighted.
    pub command_index: usize,
    /// Set when escape closes the slash-command preview. It stays closed
    /// until the prompt changes again.
    pub command_dismissed: bool,
    /// Sessions available for switching.
    pub sessions: Vec<(String, String)>,
    /// Which session is highlighted in the sessions popup.
    pub session_index: usize,
    /// The current session id.
    pub session_id: String,
    /// Set when the user asked to quit.
    pub should_quit: bool,
    /// The agent's usage for the last turn.
    pub last_usage: Option<crate::provider::types::Usage>,
    /// The runner driving sessions.
    runner: Option<Arc<SessionRunner>>,
    /// Where agent events are sent.
    event_tx: Option<mpsc::Sender<AgentEvent>>,
    /// Where agent events arrive.
    event_rx: Option<mpsc::Receiver<AgentEvent>>,
    /// Where approval requests are sent.
    approval_tx: Option<mpsc::Sender<PendingApproval>>,
    /// Where approval requests arrive.
    approval_rx: Option<mpsc::Receiver<PendingApproval>>,
    /// Where the answers of scheduled work arrive.
    scheduled_rx: Option<mpsc::Receiver<crate::schedule::Delivery>>,
    /// The list of things the assistant is working through.
    pub plan: Vec<crate::tool::todo::TodoItem>,
    /// Pictures waiting to go out with the next message.
    pub pending_images: Vec<(String, crate::image::Image)>,
    /// The colours everything is drawn in.
    pub theme: Theme,
    /// Counts the heartbeats of the interface, so the animation can turn.
    pub tick: u64,
    /// What the voice is doing.
    pub voice_state: VoiceState,
    /// The levels of the last moments, for the meter.
    pub voice_levels: Vec<f32>,
    /// When the recording that is going on began.
    ///
    /// The clock is kept here rather than in the voice, so that it goes on
    /// even when the sound is quiet and no level is reported.
    voice_start: Option<Instant>,
    /// Whether the assistant's replies are said aloud.
    pub speak_replies: bool,
    /// Whether the interface listens, answers aloud, and listens again.
    pub handsfree: bool,
    /// Whether the microphone stays open while the answer is being said.
    ///
    /// This is live mode: everything that is said is meant for lia, and
    /// talking over her stops the answer instead of waiting for it.
    pub live: bool,
    /// What the model is shown besides the voice while live.
    pub live_share: LiveShare,
    /// The words that were said a moment ago, so that the open microphone
    /// hearing them back is not taken for a person interrupting.
    recent_spoken: VecDeque<String>,
    /// Whether the rest of the answer should stay silent, because the
    /// person talked over it.
    speech_muted: bool,
    /// What was said while the agent was still working. It is sent as soon
    /// as the turn is over, so nothing is lost by interrupting.
    pending_live_message: Option<String>,
    /// Whether the microphone is open. It is kept apart from what is on
    /// screen, because in live mode the microphone and the speaker work at
    /// the same time.
    listening: bool,
    /// Whether the words go into the message box rather than being sent.
    pub dictate: bool,
    /// The voice, if this machine can hear and speak.
    voice: Option<Arc<Voice>>,
    /// The voice worker, which does the listening and the speaking.
    pub voice_worker: Option<VoiceWorker>,
    /// How long a silence ends a recording, in milliseconds.
    voice_silence_ms: u64,
}

impl App {
    pub fn new(config: Config) -> Self {
        // A session id of the form `channel:conversation` belongs to a
        // chat, so the terminal's own session does not use one: the work
        // the assistant schedules here has no chat to answer in.
        let session_id = format!("tui-{}", uuid::Uuid::new_v4());
        let theme = Theme::for_kind(config.theme);
        // The voice is looked for on this machine, so the interface can
        // say what it found.
        let voice = config
            .voice
            .enabled
            .then(|| Arc::new(Voice::new(config.voice.clone())));
        let voice_silence_ms = config.voice.silence_ms.unwrap_or(1500);
        let live_share = config.live.share;
        App {
            config,
            transcript: vec![],
            input: Input::new(),
            input_width: 40,
            scroll: 0,
            follow: true,
            scroll_max: 0,
            view_rows: 0,
            history: Vec::new(),
            history_index: None,
            history_draft: None,
            popup: Popup::None,
            busy: false,
            streaming: String::new(),
            thinking: String::new(),
            current_tools: Vec::new(),
            status: "ready".into(),
            action_started: None,
            speech_queue: VecDeque::new(),
            speech_in_flight: false,
            speech_buffer: String::new(),
            composer: SpeechComposer::new(),
            command_index: 0,
            command_dismissed: false,
            sessions: Vec::new(),
            session_index: 0,
            session_id,
            should_quit: false,
            last_usage: None,
            runner: None,
            event_tx: None,
            event_rx: None,
            approval_tx: None,
            approval_rx: None,
            scheduled_rx: None,
            plan: Vec::new(),
            pending_images: Vec::new(),
            theme,
            tick: 0,
            voice_state: VoiceState::Idle,
            voice_levels: Vec::new(),
            voice_start: None,
            // The terminal stays quiet by default: reading is faster than
            // listening, and the voice is turned on with /voice on when it
            // is wanted.
            speak_replies: false,
            handsfree: false,
            live: false,
            live_share,
            recent_spoken: VecDeque::new(),
            speech_muted: false,
            pending_live_message: None,
            listening: false,
            dictate: false,
            voice,
            voice_worker: None,
            voice_silence_ms,
        }
    }

    /// Build the agent and start listening for its events.
    pub async fn prepare(&mut self) -> anyhow::Result<()> {
        // The scheduler is built after the agent, but the agent's tools need
        // a place to find it, so they are given a slot to look in.
        let slot = crate::tool::schedule::SchedulerSlot::new();
        let agent =
            crate::cli::build_agent(&self.config, self.config.tools_enabled, Some(slot.clone()))
                .await?;
        let sessions = Arc::new(tokio::sync::Mutex::new(SessionStore::open(
            self.config.state_dir.join("sessions"),
        )?));

        // Approval requests travel from the agent to the interface, and the
        // answer travels back. The agent task waits on the answer, so the
        // request is only raised when somebody is there to see it.
        let (approval_tx, approval_rx) = mpsc::channel::<PendingApproval>(16);
        self.approval_tx = Some(approval_tx.clone());
        self.approval_rx = Some(approval_rx);

        let runner = SessionRunner::new(agent, sessions.clone());
        runner
            .set_approver(&self.session_id, Arc::new(TuiApprover { tx: approval_tx }))
            .await;

        // The work the assistant promises to do later. What it says comes
        // back to the transcript.
        let store = crate::schedule::JobStore::open(self.config.state_dir.join("schedule"))?;
        let scheduler = Arc::new(crate::schedule::Scheduler::new(store, runner.clone()));
        slot.set(scheduler.clone());
        let _scheduler = scheduler.clone().spawn();

        let (scheduled_tx, scheduled_rx) = mpsc::channel::<crate::schedule::Delivery>(16);
        scheduler.set_local_sink(scheduled_tx);
        self.scheduled_rx = Some(scheduled_rx);

        // Collect the session list for the /sessions popup.
        {
            let store = sessions.lock().await;
            self.sessions = store
                .list()
                .into_iter()
                .map(|s| (s.id.clone(), s.title.clone()))
                .collect();
        }

        // One queue carries the agent's events to the main loop.
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(512);
        self.event_tx = Some(event_tx);
        self.event_rx = Some(event_rx);

        // The voice works on its own task, so the interface never waits for
        // a recording.
        self.start_voice();

        self.runner = Some(runner);
        Ok(())
    }

    /// Start the voice, if this machine can hear and speak.
    ///
    /// The voice works on its own task, so the interface never waits for a
    /// recording.
    pub fn start_voice(&mut self) {
        if let Some(voice) = self.voice.clone() {
            self.voice_worker = Some(VoiceWorker::start(voice));
        }
    }

    /// The main loop: render, handle input, apply agent events.
    pub async fn run<B: Backend<Error = std::io::Error>>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> anyhow::Result<()> {
        self.prepare().await?;

        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // `event::read` is blocking, so it runs on its own thread and hands
        // events over a channel.
        let (key_tx, mut key_rx) = mpsc::channel::<Event>(64);
        std::thread::spawn(move || {
            while let Ok(event) = event::read() {
                if key_tx.blocking_send(event).is_err() {
                    break;
                }
            }
        });

        // The screen is redrawn only when something changed. Redrawing on
        // every tick would flood the terminal with frames nobody sees.
        let mut dirty = true;

        loop {
            if dirty {
                render::draw(terminal, self)?;
                dirty = false;
            }

            tokio::select! {
                _ = tick.tick() => {
                    // The spinner only turns while something is going on.
                    if self.busy {
                        self.tick += 1;
                        dirty = true;
                    }
                    // The clock of the recording goes on by itself, so the
                    // screen is redrawn while the microphone is open.
                    if self.is_listening() {
                        dirty = true;
                    }
                }

                // Terminal input.
                Some(event) = key_rx.recv() => {
                    // Not every event changes the screen. The mouse
                    // moving does not: crossterm reports every motion,
                    // and redrawing for each one would flood the
                    // terminal with frames nobody sees.
                    let mut redraw = true;
                    match event {
                        Event::Key(key) => {
                            if super::handle_key(self, key).await {
                                break;
                            }
                        }
                        // A paste can hold several lines, so it goes into the
                        // buffer as one piece rather than as a stream of key
                        // presses.
                        Event::Paste(text) => {
                            self.input.insert_str(&text.replace('\r', ""));
                        }
                        Event::Resize(_, _) => {}
                        // The wheel scrolls the transcript.
                        Event::Mouse(mouse) => {
                            if matches!(
                                mouse.kind,
                                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                            ) {
                                super::handle_mouse(self, mouse.kind);
                            } else {
                                redraw = false;
                            }
                        }
                        _ => {}
                    }
                    if self.should_quit {
                        break;
                    }
                    dirty = dirty || redraw;
                }

                // Agent events.
                Some(event) = async {
                    match &mut self.event_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.apply_event(event).await;
                    dirty = true;
                }

                // A tool wants permission.
                Some(pending) = async {
                    match &mut self.approval_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.status = "waiting for permission".into();
                    self.action_started = Some(Instant::now());
                    self.popup = Popup::Approval(pending);
                    dirty = true;
                }

                // The voice: a level, a recording, or words.
                Some(event) = async {
                    match &mut self.voice_worker {
                        Some(worker) => worker.events.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.apply_voice_event(event).await;
                    dirty = true;
                }

                // Work the assistant promised to do later has finished.
                Some(delivery) = async {
                    match &mut self.scheduled_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.transcript.push(Line::Notice(format!(
                        "scheduled work finished: {}",
                        delivery.prompt
                    )));
                    for line in delivery.answer.lines() {
                        self.transcript.push(Line::Assistant(line.to_string()));
                    }
                    self.status = "ready".into();
                    dirty = true;
                }
            }
        }

        Ok(())
    }

    /// Apply an agent event to the state.
    pub async fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Started { model } => {
                self.busy = true;
                self.streaming.clear();
                self.thinking.clear();
                self.current_tools.clear();
                self.status = format!("thinking ({model})");
                self.action_started = Some(Instant::now());
                self.speech_buffer.clear();
                self.composer.begin();
                // A new turn is a new answer: what was said before it is
                // not an echo of this one.
                self.recent_spoken.clear();
                self.speech_muted = false;
            }
            AgentEvent::TextDelta(text) => {
                self.streaming.push_str(&text);
                self.feed_speech(&text);
            }
            AgentEvent::ThinkingDelta(text) => {
                self.thinking.push_str(&text);
            }
            AgentEvent::ToolCallStarted { id, name, input } => {
                let input_text = serde_json::to_string_pretty(&input)
                    .unwrap_or_default()
                    .lines()
                    .take(6)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.status = format!("running {name}");
                self.action_started = Some(Instant::now());
                self.current_tools.push(Line::Tool {
                    id,
                    name,
                    input: input_text,
                    result: None,
                });
            }
            AgentEvent::ToolCallFinished { id, name, output } => {
                // The plan the assistant is working through, if it wrote one.
                if name == "todo" {
                    self.plan = plan_of(&output);
                }
                // Fill in the result of the matching tool call.
                for line in self.current_tools.iter_mut() {
                    if let Line::Tool {
                        id: line_id,
                        result,
                        ..
                    } = line
                        && *line_id == id
                    {
                        *result = Some(ToolResultView {
                            content: output.content,
                            is_error: output.is_error,
                        });
                        break;
                    }
                }
                let _ = name;
                self.status = "thinking".into();
                self.action_started = Some(Instant::now());
            }
            AgentEvent::SubAssistant { label, note } => {
                // What the other assistant is doing, shown under the work
                // that was handed over.
                self.action_started = Some(Instant::now());
                self.current_tools.push(Line::Sub { label, note });
            }
            AgentEvent::TurnComplete { .. } => {}
            AgentEvent::Finished { usage, .. } => {
                self.busy = false;
                self.last_usage = usage;
                self.status = "ready".into();
                self.action_started = None;
                // The answer has been said sentence by sentence while it
                // arrived. Only the unfinished tail is left to say.
                self.flush_speech();
                self.flush_turn();
                if self.live && self.pending_live_message.is_some() {
                    // What was said over the answer was kept, and is sent
                    // now that the turn is over.
                    let text = self.pending_live_message.take().unwrap_or_default();
                    self.speech_muted = false;
                    if !text.is_empty() {
                        self.input.set_text(&text);
                        self.submit();
                    }
                }
                self.listen_after_speech();
                if self.live {
                    self.ensure_listening();
                }
            }
            AgentEvent::Notice(message) => {
                self.transcript.push(Line::Notice(message));
            }
            AgentEvent::Error(message) => {
                self.busy = false;
                self.status = "ready".into();
                self.action_started = None;
                self.speech_buffer.clear();
                self.composer.begin();
                self.transcript.push(Line::Error(message));
                self.flush_turn();
                if self.live && self.pending_live_message.is_some() {
                    let text = self.pending_live_message.take().unwrap_or_default();
                    self.speech_muted = false;
                    if !text.is_empty() {
                        self.input.set_text(&text);
                        self.submit();
                    }
                }
                self.listen_after_speech();
                if self.live {
                    self.ensure_listening();
                }
            }
        }
    }

    /// Move the in-progress turn into the transcript.
    fn flush_turn(&mut self) {
        // The tools ran before the final text, so they are listed first.
        for tool in std::mem::take(&mut self.current_tools) {
            self.transcript.push(tool);
        }
        if !self.thinking.is_empty() {
            self.transcript
                .push(Line::Thinking(std::mem::take(&mut self.thinking)));
        }
        if !self.streaming.is_empty() {
            self.transcript
                .push(Line::Assistant(std::mem::take(&mut self.streaming)));
        }
    }

    /// The slash-command suggestions for what is in the prompt now.
    ///
    /// The preview is only for naming a command: as soon as there is a
    /// space, the person is typing arguments and the list gets out of the
    /// way. It is also hidden while another overlay owns the screen.
    pub fn command_matches(&self) -> Vec<&'static commands::CommandSpec> {
        if self.command_dismissed
            || self.popup != Popup::None
            || (self.is_listening() && !self.live)
        {
            return Vec::new();
        }
        commands::matching(self.input.text())
    }

    /// Move the highlight through the command preview, wrapping at the ends.
    pub fn move_command_selection(&mut self, delta: isize) {
        let count = self.command_matches().len();
        if count == 0 {
            return;
        }
        self.command_index =
            (self.command_index as isize + delta).rem_euclid(count as isize) as usize;
    }

    /// Put the highlighted command in the prompt, ready for its arguments.
    pub fn complete_command(&mut self) -> bool {
        let matches = self.command_matches();
        if matches.is_empty() {
            return false;
        }
        let command = matches[self.command_index.min(matches.len() - 1)];
        self.input.set_text(&format!("{} ", command.name));
        self.command_index = 0;
        self.command_dismissed = false;
        true
    }

    /// Run the highlighted command.
    pub fn run_selected_command(&mut self) -> bool {
        let matches = self.command_matches();
        if matches.is_empty() {
            return false;
        }
        let command = matches[self.command_index.min(matches.len() - 1)];
        self.input.set_text(command.name);
        self.command_index = 0;
        self.submit();
        true
    }

    /// Close the preview without changing the prompt.
    pub fn dismiss_command_preview(&mut self) {
        self.command_dismissed = true;
        self.command_index = 0;
    }

    /// A new character or deletion resets the preview and its highlight.
    pub fn reset_command_preview(&mut self) {
        self.command_index = 0;
        self.command_dismissed = false;
    }

    /// What the assistant is doing, in a few words for the activity rail.
    ///
    /// A running tool is more useful than the model name, so it comes first;
    /// otherwise the turn's phase is the answer. This is the phrase the
    /// interface animates, not a log line.
    pub fn current_action(&self) -> String {
        if let Some(Line::Tool { name, .. }) = self
            .current_tools
            .iter()
            .rev()
            .find(|line| matches!(line, Line::Tool { result: None, .. }))
        {
            return format!("running {name}");
        }
        if !self.streaming.trim().is_empty() {
            return "writing the answer".into();
        }
        if !self.thinking.trim().is_empty() {
            return "reasoning".into();
        }
        self.status.clone()
    }

    /// How long the action now on screen has been going, in seconds.
    pub fn action_elapsed(&self) -> f64 {
        self.action_started
            .map(|start| start.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    /// Submit the current input.
    pub fn submit(&mut self) {
        if self.busy {
            self.transcript
                .push(Line::Notice("still working on the previous message".into()));
            return;
        }

        let text = self.input.take();
        if text.is_empty() && self.pending_images.is_empty() {
            return;
        }

        // A slash command is handled locally.
        if text.starts_with('/') {
            self.run_command(&text);
            self.history.push(text.clone());
            self.history_index = None;
            self.history_draft = None;
            return;
        }

        if !text.is_empty() {
            self.history.push(text.clone());
            self.history_index = None;
            self.history_draft = None;
        }

        // The pictures go out with this message, and are not sent twice.
        let pictures: Vec<crate::provider::types::ContentBlock> =
            std::mem::take(&mut self.pending_images)
                .into_iter()
                .map(|(_, image)| crate::provider::types::ContentBlock::Image {
                    media_type: image.media_type,
                    data: image.data,
                })
                .collect();

        if !text.is_empty() {
            self.transcript.push(Line::User(text.clone()));
        }
        self.scroll_to_bottom();

        let message = if pictures.is_empty() {
            crate::provider::types::Message::user(text)
        } else {
            crate::provider::types::Message::user_with_images(text, pictures)
        };

        // The turn runs in the background; its events come back on the queue
        // the main loop already reads.
        let runner = self.runner.clone();
        let session_id = self.session_id.clone();
        let event_tx = self.event_tx.clone();

        if let (Some(runner), Some(event_tx)) = (runner, event_tx) {
            tokio::spawn(async move {
                runner
                    .run_message(&session_id, Some(message), event_tx)
                    .await;
            });
        }
    }

    /// Handle a slash command.
    fn run_command(&mut self, text: &str) {
        let mut parts = text.split_whitespace();
        let command = parts.next().unwrap_or("");
        let arg = parts.collect::<Vec<_>>().join(" ");

        match command {
            "/help" => {
                self.toggle_help();
            }
            "/image" => {
                if arg.is_empty() {
                    self.transcript
                        .push(Line::Notice("which picture? /image <path>".into()));
                } else {
                    self.attach_image(&arg);
                }
            }
            "/clear" => {
                self.transcript.clear();
                self.follow = true;
                // The transcript is only what is on screen; the session is
                // what the model remembers. Both are cleared, so the next
                // message starts a fresh conversation.
                if let Some(runner) = self.runner.clone() {
                    let session_id = self.session_id.clone();
                    tokio::spawn(async move {
                        runner.reset_session(&session_id).await;
                    });
                }
            }
            "/config" => {
                self.config_command(&arg);
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
            }
            "/model" => {
                if arg.is_empty() {
                    let model = self.config.effective_model();
                    self.transcript
                        .push(Line::Notice(format!("model: {model}")));
                } else {
                    self.config.model = Some(arg.clone());
                    self.transcript
                        .push(Line::Notice(format!("model set to {arg}")));
                }
            }
            "/tools" => {
                let tools = crate::cli::build_tools(&self.config);
                let names = tools.names().join(", ");
                self.transcript
                    .push(Line::Notice(format!("tools: {names}")));
            }
            "/skills" => {
                let skills = self.config.discover_skills();
                self.transcript.push(Line::Notice(skills.summary()));
            }
            "/new" => {
                let id = format!("tui-{}", uuid::Uuid::new_v4());
                if let Some(runner) = self.runner.clone() {
                    let tx = self.approval_tx.clone();
                    let id2 = id.clone();
                    tokio::spawn(async move {
                        let _ = runner.create_session(&id2).await;
                        if let Some(tx) = tx {
                            runner
                                .set_approver(&id2, Arc::new(TuiApprover { tx }))
                                .await;
                        }
                    });
                }
                self.adopt_new_session(id);
                self.transcript
                    .push(Line::Notice("a new session is open".into()));
            }
            "/sessions" => {
                self.open_sessions();
            }
            "/listen" => {
                self.start_listening(false);
            }
            "/dictate" => {
                self.start_listening(true);
            }
            "/speak" => {
                if arg.is_empty() {
                    self.transcript
                        .push(Line::Notice("what should it say? /speak <words>".into()));
                } else {
                    self.speak(&arg);
                }
            }
            "/voice" => match arg.as_str() {
                "on" => {
                    self.speak_replies = true;
                    self.transcript
                        .push(Line::Notice("the answers will be said aloud".into()));
                }
                "off" => {
                    self.speak_replies = false;
                    self.transcript
                        .push(Line::Notice("the answers will be silent".into()));
                }
                _ => {
                    let capabilities = self.voice_capabilities();
                    let mut note = vec![format!(
                        "voice: {}",
                        if self.speak_replies { "on" } else { "off" }
                    )];
                    note.push(match &capabilities.recorder {
                        Some(program) => format!("the microphone is recorded by {program}"),
                        None => "no recorder was found".to_string(),
                    });
                    note.push(match &capabilities.transcriber {
                        Some(program) => format!("the words come from {program}"),
                        None => "no transcriber was found".to_string(),
                    });
                    note.push(match &capabilities.speaker {
                        Some(program) => match &capabilities.voice_model {
                            Some(model) => format!("the voice is {program} ({model})"),
                            None => format!("the voice is {program}"),
                        },
                        None => "no speaker was found".to_string(),
                    });
                    self.transcript.push(Line::Notice(note.join(" · ")));
                }
            },
            "/handsfree" => match arg.as_str() {
                "on" => {
                    self.handsfree = true;
                    self.live = false;
                    self.transcript.push(Line::Notice(format!(
                        "hands-free is on — say \"{}\" first, and the answer is said aloud",
                        self.wake_word()
                    )));
                    self.start_listening(false);
                }
                "off" => {
                    self.handsfree = false;
                    self.cancel_listening();
                    self.transcript
                        .push(Line::Notice("hands-free is off".into()));
                }
                _ => {
                    let state = if self.handsfree { "on" } else { "off" };
                    self.transcript
                        .push(Line::Notice(format!("hands-free is {state}")));
                }
            },
            "/live" => match arg.as_str() {
                "on" => {
                    self.start_live();
                }
                "off" => {
                    self.live = false;
                    self.pending_live_message = None;
                    self.speech_muted = false;
                    self.cancel_listening();
                    self.stop_speaking();
                    self.transcript.push(Line::Notice("live is off".into()));
                }
                "" => {
                    let state = if self.live { "on" } else { "off" };
                    self.transcript.push(Line::Notice(format!(
                        "live is {state} · sharing {} · echo guard {:.2}",
                        self.live_share.as_str(),
                        self.config.live.echo_similarity
                    )));
                }
                other => match other.parse::<LiveShare>() {
                    Ok(share) => {
                        self.live_share = share;
                        if share == LiveShare::None {
                            self.transcript
                                .push(Line::Notice("live is voice only".into()));
                        } else {
                            self.start_live();
                        }
                    }
                    Err(e) => self.transcript.push(Line::Error(e)),
                },
            },
            other => {
                self.transcript
                    .push(Line::Notice(format!("unknown command: {other}")));
            }
        }
    }

    /// Show the settings, or change one and write it back to the file.
    ///
    /// The displayed names are the same ones the browser's configuration
    /// panel uses, so a setting has one name everywhere.
    fn config_command(&mut self, arg: &str) {
        let mut pieces = arg.splitn(2, char::is_whitespace);
        let action = pieces.next().unwrap_or("");
        if action.is_empty() {
            self.transcript.push(Line::Notice(
                "configuration — /config set <key> <value>".into(),
            ));
            for (key, value) in self.config.settings() {
                self.transcript
                    .push(Line::Notice(format!("  {key:<18} {value}")));
            }
            self.transcript.push(Line::Notice(format!(
                "  {:<18} {}",
                "file",
                self.config.configured_path().display()
            )));
            return;
        }
        if action == "path" {
            self.transcript.push(Line::Notice(format!(
                "configuration file: {}",
                self.config.configured_path().display()
            )));
            return;
        }
        if action != "set" {
            self.transcript
                .push(Line::Notice("usage: /config [set <key> <value>]".into()));
            return;
        }

        let rest = pieces.next().unwrap_or("").trim();
        let mut pair = rest.splitn(2, char::is_whitespace);
        let key = pair.next().unwrap_or("").trim();
        let value = pair.next().unwrap_or("").trim();
        if key.is_empty() {
            self.transcript
                .push(Line::Notice("usage: /config set <key> <value>".into()));
            return;
        }
        if let Err(e) = self.config.set_setting(key, value) {
            self.transcript.push(Line::Error(e.to_string()));
            return;
        }

        match self.config.save_configured() {
            Ok(path) => {
                self.transcript
                    .push(Line::Notice(format!("{key} saved to {}", path.display())));
                if key == "theme" {
                    self.theme = Theme::for_kind(self.config.theme);
                } else {
                    self.transcript.push(Line::Notice(
                        "restart lia to apply provider, model, tool, voice and agent settings"
                            .into(),
                    ));
                }
            }
            Err(e) => self
                .transcript
                .push(Line::Error(format!("the configuration was not saved: {e}"))),
        }
    }

    /// Attach a picture to the next message.
    fn attach_image(&mut self, path: &str) {
        let path = std::path::Path::new(path);
        match crate::image::Image::from_file(path) {
            Ok(image) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                let note = format!(
                    "{name} ({}, {} kB)",
                    image.media_type,
                    image.data.len() * 3 / 4 / 1024
                );
                self.pending_images.push((name.clone(), image));
                self.transcript.push(Line::Notice(format!(
                    "picture attached: {note} — it goes with the next message"
                )));
            }
            Err(e) => self.transcript.push(Line::Error(e)),
        }
    }

    /// Take the one picture that goes with a live message.
    ///
    /// The camera or the screen is asked for a frame at the moment the
    /// person spoke, rather than a stream: it is what the model is shown
    /// along with the words. A picture that cannot be taken is reported and
    /// the words still go out.
    async fn attach_live_frame(&mut self) {
        if self.live_share == LiveShare::None {
            return;
        }
        match crate::live::capture_frame(&self.config.live, self.live_share).await {
            Ok(image) => {
                let name = format!("live {}", self.live_share.as_str());
                self.pending_images.push((name, image));
            }
            Err(e) => self.transcript.push(Line::Error(format!(
                "the live picture could not be taken: {e}"
            ))),
        }
    }

    /// Close any open popup.
    pub fn dismiss_popup(&mut self) {
        self.popup = Popup::None;
    }

    /// What the escape key does: a recording is thrown away, and anything
    /// else is a popup that closes.
    pub fn escape(&mut self) {
        if !self.command_matches().is_empty() {
            self.dismiss_command_preview();
            return;
        }
        // In live mode the microphone is meant to stay open, so escape is
        // what stops the answer rather than the recording.
        if self.live && self.is_speaking() {
            self.speech_muted = true;
            self.stop_speaking();
            self.transcript
                .push(Line::Notice("the answer was stopped".into()));
            return;
        }
        if self.is_listening() && !self.live {
            self.cancel_listening();
            self.transcript
                .push(Line::Notice("the recording was thrown away".into()));
            return;
        }
        self.dismiss_popup();
    }

    /// Show or hide the help overlay.
    pub fn toggle_help(&mut self) {
        self.popup = if self.popup == Popup::Help {
            Popup::None
        } else {
            Popup::Help
        };
    }

    /// Show the session list, with the current session highlighted.
    pub fn open_sessions(&mut self) {
        // The list may have changed since it was last read.
        self.session_index = self
            .sessions
            .iter()
            .position(|(id, _)| *id == self.session_id)
            .unwrap_or(0);
        self.popup = Popup::Sessions;
    }

    /// Move the highlight in the session list.
    pub fn move_session_selection(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            return;
        }
        let last = self.sessions.len() - 1;
        let next = self.session_index as isize + delta;
        self.session_index = next.clamp(0, last as isize) as usize;
    }

    /// Switch to the highlighted session and show its transcript.
    pub async fn switch_session(&mut self) {
        let Some((id, title)) = self.sessions.get(self.session_index).cloned() else {
            self.dismiss_popup();
            return;
        };

        if id == self.session_id {
            self.dismiss_popup();
            return;
        }

        // The new session needs its own permission handler, or a tool in it
        // would wait for an answer that never comes.
        if let (Some(runner), Some(tx)) = (self.runner.clone(), self.approval_tx.clone()) {
            runner.set_approver(&id, Arc::new(TuiApprover { tx })).await;
        }

        let messages = match &self.runner {
            Some(runner) => runner.session_messages(&id).await,
            None => None,
        };

        self.transcript = messages
            .as_deref()
            .map(lines_from_messages)
            .unwrap_or_default();
        self.session_id = id.clone();
        self.current_tools.clear();
        self.streaming.clear();
        self.popup = Popup::None;
        self.status = format!("session: {title}");
        self.scroll_to_bottom();
    }

    /// Create a fresh session, switch to it, and leave the transcript empty.
    pub async fn create_session(&mut self) {
        let id = format!("tui-{}", uuid::Uuid::new_v4());
        if let Some(runner) = self.runner.clone() {
            let _ = runner.create_session(&id).await;
            if let Some(tx) = self.approval_tx.clone() {
                runner
                    .set_approver(&id, Arc::new(TuiApprover { tx }))
                    .await;
            }
        }
        self.adopt_new_session(id);
    }

    /// Another spelling for the same action.
    pub async fn new_session(&mut self) {
        self.create_session().await;
    }

    /// Common tail of session creation, used by both the async path and the
    /// slash command (which cannot await the runner).
    fn adopt_new_session(&mut self, id: String) {
        self.sessions.insert(0, (id.clone(), "New session".to_string()));
        self.session_index = 0;
        self.session_id = id;
        self.transcript.clear();
        self.current_tools.clear();
        self.streaming.clear();
        self.popup = Popup::None;
        self.status = "session: New session".into();
        self.follow = true;
        self.scroll_to_bottom();
    }

    /// Answer a pending approval request and let the agent carry on.
    pub fn resolve_approval(&mut self, decision: ApprovalDecision) {
        if let Popup::Approval(pending) = &mut self.popup {
            pending.answer(decision);
        }
        self.popup = Popup::None;
        self.status = if self.busy {
            "working".into()
        } else {
            "ready".into()
        };
        if self.busy {
            self.action_started = Some(Instant::now());
        }
    }

    /// Type a character.
    pub fn insert(&mut self, c: char) {
        self.input.insert_char(c);
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        self.input.backspace();
    }

    /// Move the cursor one character to the left.
    pub fn cursor_left(&mut self) {
        self.input.move_left();
    }

    /// Move the cursor one character to the right.
    pub fn cursor_right(&mut self) {
        self.input.move_right();
    }

    /// Move the cursor to the start of the line.
    pub fn cursor_home(&mut self) {
        self.input.move_to_line_start();
    }

    /// Move the cursor to the end of the line.
    pub fn cursor_end(&mut self) {
        self.input.move_to_line_end();
    }

    /// The input text wrapped into the rows the box shows.
    pub fn visual_lines(&self) -> Vec<wrap::VisualLine> {
        wrap::wrap(self.input.text(), self.input_width.max(1) as usize)
    }

    /// Move the cursor a row of the wrapped text up or down.
    ///
    /// It returns false when there is no row to go to, so the caller can
    /// reach for the history instead.
    pub fn move_visual_line(&mut self, delta: isize) -> bool {
        let lines = self.visual_lines();
        if lines.is_empty() {
            return false;
        }
        let spot = wrap::cursor_spot(&lines, self.input.cursor());
        let target = spot.line as isize + delta;
        if target < 0 || target as usize >= lines.len() {
            return false;
        }
        // The column the cursor would like to be in is remembered, so
        // moving down a long row and back up again lands where it started.
        let goal = self.input.goal_column().unwrap_or(spot.column);
        let index = wrap::column_to_char(&lines[target as usize], goal);
        self.input.set_goal_column(Some(goal));
        self.input.set_cursor_at(index);
        true
    }

    /// Recall the previous input.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        // The first time the history is reached, the draft is kept aside.
        if self.history_index.is_none() {
            self.history_draft = Some((self.input.text().to_string(), self.input.cursor()));
        }
        let index = match self.history_index {
            Some(i) => i.saturating_sub(1),
            None => self.history.len() - 1,
        };
        self.history_index = Some(index);
        self.input.set_text(&self.history[index]);
    }

    /// Recall the next input.
    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            // The draft comes back.
            match self.history_draft.take() {
                Some((text, cursor)) => self.input.set_text_at(&text, cursor),
                None => self.input.clear(),
            }
            return;
        }
        self.history_index = Some(index + 1);
        self.input.set_text(&self.history[index + 1]);
    }

    /// Scroll back by so many rows, and stop following the newest output.
    ///
    /// The view stays where it is put: the words that arrive while the
    /// person is reading something older land below the view, rather
    /// than dragging it forward.
    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
        self.follow = false;
    }

    /// Scroll forward by so many rows.
    ///
    /// The view follows the newest output again once it reaches the
    /// bottom, which the renderer notices.
    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_add(rows);
        self.follow = false;
    }

    /// Scroll to the oldest output.
    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow = false;
    }

    /// Scroll to the newest output, and follow it from now on.
    pub fn scroll_to_bottom(&mut self) {
        self.follow = true;
    }

    /// How many rows a page is: half the panel, and never fewer than
    /// five.
    pub fn page_rows(&self) -> usize {
        (self.view_rows / 2).max(5)
    }

    /// Complete the current input. No completion sources yet, so this only
    /// accepts a tab as a no-op.
    pub fn complete(&mut self) {}

    /// Whether the app is showing an approval prompt.
    pub fn is_awaiting_approval(&self) -> bool {
        matches!(self.popup, Popup::Approval(_))
    }

    /// Whether this machine can hear and speak.
    pub fn has_voice(&self) -> bool {
        self.voice.is_some()
    }

    /// What the voice can do, for the commands that report it.
    pub fn voice_capabilities(&self) -> crate::voice::Capabilities {
        self.voice
            .as_ref()
            .map(|voice| voice.capabilities())
            .unwrap_or_default()
    }

    /// Whether the microphone is open.
    pub fn is_listening(&self) -> bool {
        self.listening
    }

    /// Whether the speaker is working.
    pub fn is_speaking(&self) -> bool {
        self.speech_in_flight
    }

    /// The word lia answers to.
    ///
    /// It is the name of lia, unless the configuration says otherwise.
    pub fn wake_word(&self) -> String {
        self.config
            .voice
            .wake_word
            .clone()
            .unwrap_or_else(|| crate::voice::WAKE_WORD.to_string())
    }

    /// Start a recording.
    ///
    /// The recording ends by itself when the person stops talking, or when
    /// the time runs out, or when it is stopped.
    pub fn start_listening(&mut self, dictate: bool) {
        let Some(worker) = self.voice_worker.as_ref() else {
            self.transcript
                .push(Line::Notice("nothing on this machine can hear".into()));
            return;
        };
        if self.listening {
            return;
        }
        // Outside live mode the agent has to finish before the next
        // question: one turn at a time. Live mode listens all the same, so
        // the person can talk over the answer.
        if self.busy && !self.live {
            self.transcript
                .push(Line::Notice("still working on the previous message".into()));
            return;
        }

        let max_seconds = self.config.voice.max_seconds.clamp(1, 600);
        let silence_ms = Some(self.voice_silence_ms);
        let _ = worker.commands.try_send(VoiceCommand::Listen {
            max_seconds,
            silence_ms,
        });
        self.dictate = dictate;
        self.listening = true;
        self.voice_levels.clear();
        self.voice_start = Some(Instant::now());
        self.voice_state = VoiceState::Listening {
            level: 0.0,
            elapsed: 0.0,
        };
        self.status = "listening".into();
    }

    /// Open the microphone again, if live mode wants it and nothing has it.
    fn ensure_listening(&mut self) {
        if self.live && !self.listening && self.voice_worker.is_some() {
            self.start_listening(false);
        }
    }

    /// Turn live mode on, and open the microphone.
    fn start_live(&mut self) {
        if self.voice_worker.is_none() {
            self.transcript
                .push(Line::Notice("nothing on this machine can hear".into()));
            return;
        }
        self.live = true;
        self.handsfree = false;
        // Live mode is a conversation: the answer is part of it, and is
        // said aloud even if the replies were switched off.
        self.speak_replies = true;
        self.speech_muted = false;
        let sharing = match self.live_share {
            LiveShare::None => "voice only".to_string(),
            LiveShare::Camera => "sharing the camera".to_string(),
            LiveShare::Screen => "sharing the screen".to_string(),
        };
        self.transcript.push(Line::Notice(format!(
            "live is on — {sharing}; the microphone stays open, and talking over the answer stops it"
        )));
        let missing = match self.live_share {
            LiveShare::Camera => self.config.live.camera_command.is_none(),
            LiveShare::Screen => self.config.live.screen_command.is_none(),
            LiveShare::None => false,
        };
        if missing {
            self.transcript.push(Line::Notice(format!(
                "no {} command is configured, so the words go out on their own",
                self.live_share.as_str()
            )));
        }
        self.ensure_listening();
    }

    /// How long the recording that is going on has been going, in seconds.
    ///
    /// The clock is read here rather than taken from the last report of
    /// the voice, so that it goes on while the sound is quiet and the
    /// voice has nothing to say.
    pub fn recording_elapsed(&self) -> f64 {
        match (&self.voice_start, &self.voice_state) {
            (Some(start), VoiceState::Listening { .. }) => start.elapsed().as_secs_f64(),
            (_, VoiceState::Listening { elapsed, .. }) => *elapsed,
            _ => 0.0,
        }
    }

    /// Stop the recording, keeping the words.
    pub fn stop_listening(&mut self) {
        if !self.is_listening() {
            return;
        }
        if let Some(worker) = self.voice_worker.as_ref() {
            let _ = worker.commands.try_send(VoiceCommand::Stop);
        }
        self.listening = false;
        self.voice_start = None;
        self.voice_state = if self.speech_in_flight {
            VoiceState::Speaking
        } else {
            VoiceState::Idle
        };
        self.status = "ready".into();
    }

    /// Stop the recording and throw the words away.
    pub fn cancel_listening(&mut self) {
        if !self.is_listening() {
            return;
        }
        self.dictate = false;
        if let Some(worker) = self.voice_worker.as_ref() {
            // The words are thrown away, not kept: the sound is stopped and
            // never turned into words.
            let _ = worker.commands.try_send(VoiceCommand::Cancel);
        }
        self.listening = false;
        self.voice_state = if self.speech_in_flight {
            VoiceState::Speaking
        } else {
            VoiceState::Idle
        };
        self.voice_levels.clear();
        self.voice_start = None;
        self.status = "ready".into();
    }

    /// Stop the words that are being said, and forget the rest.
    ///
    /// The microphone is left alone: in live mode it is open while she
    /// speaks, and it is the microphone that heard the interruption.
    pub fn stop_speaking(&mut self) {
        if let Some(worker) = self.voice_worker.as_ref() {
            let _ = worker.commands.try_send(VoiceCommand::StopSpeaking);
        }
        self.speech_queue.clear();
        self.speech_in_flight = false;
        if !self.listening {
            self.voice_state = VoiceState::Idle;
        }
    }

    /// Start or stop the recording, depending on what is going on.
    pub fn toggle_listening(&mut self) {
        if self.is_listening() {
            self.stop_listening();
        } else {
            self.start_listening(false);
        }
    }

    /// Say something out loud.
    ///
    /// The text joins the speech queue. The agent's answer is fed here a
    /// sentence at a time as it streams, so the first words are heard
    /// without waiting for the last ones to be generated.
    pub fn speak(&mut self, text: &str) {
        if self.voice_worker.is_none() {
            self.transcript
                .push(Line::Notice("nothing on this machine can speak".into()));
            return;
        }
        if text.trim().is_empty() {
            return;
        }
        // A line asked for by hand is said as it is: the marks come off,
        // but no courtesy is added to it.
        let words = self.composer.direct(text);
        self.push_spoken(words);
    }

    /// Feed the part of an answer that has just arrived into the speaker.
    fn feed_speech(&mut self, text: &str) {
        if self.speech_muted
            || !(self.speak_replies || self.handsfree || self.live)
            || self.voice_worker.is_none()
        {
            return;
        }
        self.speech_buffer.push_str(text);
        loop {
            if let Some(end) = speech_boundary(&self.speech_buffer) {
                let chunk = self.speech_buffer[..end].to_string();
                self.speech_buffer.drain(..end);
                self.speak_chunk(&chunk);
                continue;
            }
            if let Some(newline) = self.speech_buffer.find('\n') {
                let line = self.speech_buffer[..newline].to_string();
                self.speech_buffer.drain(..=newline);
                self.speak_line(&line);
                continue;
            }
            if self.composer.in_code() {
                break;
            }
            if self.speech_buffer.chars().count() > 400 {
                let at = self
                    .speech_buffer
                    .char_indices()
                    .take_while(|(index, _)| *index <= 380)
                    .filter(|(_, ch)| ch.is_whitespace())
                    .map(|(index, ch)| index + ch.len_utf8())
                    .last();
                if let Some(at) = at {
                    let chunk = self.speech_buffer[..at].to_string();
                    self.speech_buffer.drain(..at);
                    self.speak_chunk(&chunk);
                    continue;
                }
            }
            break;
        }
    }

    /// Turn one complete line of the answer into speech, unless it is
    /// code.
    fn speak_line(&mut self, line: &str) {
        for said in self.composer.line(line) {
            self.push_spoken(said);
        }
    }

    /// Compose one sentence of prose and put it on the speech queue.
    fn speak_chunk(&mut self, chunk: &str) {
        let said = self.composer.spoken_chunk(chunk);
        self.push_spoken(said);
    }

    /// Say whatever is left when the answer is complete.
    fn flush_speech(&mut self) {
        let rest = std::mem::take(&mut self.speech_buffer);
        // The person talked over the answer, so the rest of it is not
        // wanted: the turn ends quietly.
        if self.speech_muted {
            self.composer.begin();
            return;
        }
        // A roster that was never followed by prose still gets said.
        for said in self.composer.flush() {
            self.push_spoken(said);
        }
        if !self.composer.in_code() {
            self.speak_chunk(&rest);
        }
    }

    /// Put a word of the composed answer on the speech queue.
    fn push_spoken(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        // The microphone may be open while this is said, so a note is kept
        // of it: what comes back through the room is not a question.
        self.recent_spoken.push_back(text.clone());
        while self.recent_spoken.len() > 16 {
            self.recent_spoken.pop_front();
        }
        self.speech_queue.push_back(text);
        self.pump_speech();
    }

    /// Give the speaker the next chunk, when it is free.
    fn pump_speech(&mut self) {
        if self.speech_in_flight {
            return;
        }
        let Some(worker) = self.voice_worker.as_ref() else {
            self.speech_queue.clear();
            return;
        };
        let text = match self.speech_queue.pop_front() {
            Some(text) => text,
            None => {
                if self.voice_state == VoiceState::Speaking && !self.listening {
                    self.voice_state = VoiceState::Idle;
                }
                return;
            }
        };
        let mut text = text;
        while let Some(next) = self.speech_queue.front() {
            if text.len() + 1 + next.len() > 800 {
                break;
            }
            let next = self.speech_queue.pop_front().unwrap();
            text.push(' ');
            text.push_str(&next);
        }
        if worker.commands.try_send(VoiceCommand::Speak(text)).is_ok() {
                self.speech_in_flight = true;
                if !self.listening {
                    self.voice_state = VoiceState::Speaking;
                }
            } else {
                self.speech_queue.clear();
            }
    }

    /// Open the microphone again once every queued word has been said.
    fn listen_after_speech(&mut self) {
        if (self.handsfree || self.live)
            && !self.listening
            && !self.speech_in_flight
            && self.speech_queue.is_empty()
            && (!self.busy || self.live)
        {
            self.start_listening(false);
        }
    }

    /// Apply a voice event to the state.
    pub async fn apply_voice_event(&mut self, event: VoiceEvent) {
        match event {
            VoiceEvent::Listening { level, elapsed } => {
                // A report that arrives after the recording was stopped is
                // of no use: the microphone is closed, and the level
                // would put the interface back into a recording that is
                // not going on.
                if !self.is_listening() {
                    return;
                }
                self.voice_levels.push(level);
                // Only the last moments are kept, for the meter.
                if self.voice_levels.len() > 24 {
                    self.voice_levels.remove(0);
                }
                self.voice_state = VoiceState::Listening { level, elapsed };
            }
            VoiceEvent::Transcript { text } => {
                self.listening = false;
                self.voice_levels.clear();
                self.voice_start = None;
                self.voice_state = if self.speech_in_flight {
                    VoiceState::Speaking
                } else {
                    VoiceState::Idle
                };
                let text = text.trim().to_string();
                if text.is_empty() {
                    self.status = if self.busy {
                        self.status.clone()
                    } else {
                        "ready".into()
                    };
                    if self.handsfree {
                        // Nothing was said, so the loop waits for the
                        // person to speak again.
                        self.start_listening(false);
                    } else if self.live {
                        self.ensure_listening();
                    }
                    return;
                }
                if self.live {
                    // The microphone is open while she speaks, so some of
                    // what it hears is her own voice coming back through
                    // the room. Words that are mostly hers are not a
                    // question.
                    if self.speech_in_flight || !self.speech_queue.is_empty() {
                        let spoken: Vec<String> = self.recent_spoken.iter().cloned().collect();
                        if crate::live::looks_like_echo(
                            &text,
                            &spoken,
                            self.config.live.echo_similarity,
                        ) {
                            self.status = "listening".into();
                            self.ensure_listening();
                            return;
                        }
                        // A person talked over the answer: the rest of it
                        // is no longer wanted.
                        self.stop_speaking();
                        self.speech_muted = true;
                        self.transcript.push(Line::Notice("interrupted".into()));
                    }
                    // One picture from the camera or the screen goes with
                    // the words, if one is being shared.
                    self.attach_live_frame().await;
                    if self.busy {
                        // The turn that is running cannot be taken back,
                        // but it can be kept quiet. The words wait for it
                        // to finish, and are sent then.
                        self.pending_live_message = Some(text);
                        self.status = "listening".into();
                        self.ensure_listening();
                        return;
                    }
                    self.input.set_text(&text);
                    self.submit();
                    self.ensure_listening();
                    return;
                }
                if self.handsfree {
                    // The room is full of talk, and only some of it is
                    // for lia: the words have to name her before she
                    // answers.
                    let Some(message) = crate::voice::addressed_to(&text, &self.wake_word()) else {
                        self.status = "waiting for lia".into();
                        self.start_listening(false);
                        return;
                    };
                    if message.is_empty() {
                        // Only her name was said, so there is nothing to
                        // answer yet.
                        self.status = "waiting for lia".into();
                        self.start_listening(false);
                        return;
                    }
                    self.input.set_text(&message);
                    self.submit();
                    return;
                }
                if self.dictate {
                    self.dictate = false;
                    self.input.set_text(&text);
                    self.transcript.push(Line::Notice(
                        "the words are in the box — edit them, then send".into(),
                    ));
                    self.status = "ready".into();
                    return;
                }
                // The words are the user's message.
                self.input.set_text(&text);
                self.submit();
            }
            VoiceEvent::Spoken => {
                self.speech_in_flight = false;
                self.pump_speech();
                if !self.speech_in_flight && self.speech_queue.is_empty() {
                    if !self.listening {
                        self.voice_state = VoiceState::Idle;
                    }
                    self.listen_after_speech();
                }
            }
            VoiceEvent::Interrupted => {
                self.speech_in_flight = false;
                self.pump_speech();
                if !self.speech_in_flight && self.speech_queue.is_empty() && !self.listening {
                    self.voice_state = VoiceState::Idle;
                }
                self.listen_after_speech();
            }
            VoiceEvent::Failed(message) => {
                self.listening = false;
                self.voice_state = VoiceState::Idle;
                self.voice_levels.clear();
                self.voice_start = None;
                self.status = "ready".into();
                self.transcript.push(Line::Error(message));
            }
        }
    }
}

/// Where a piece of prose ends, for the speaker.
///
/// A full stop is only a boundary when a space follows it, so a version
/// number or a path does not split the sentence in two.
fn speech_boundary(text: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (index, (byte, ch)) in chars.iter().enumerate() {
        if !matches!(ch, '.' | '!' | '?') {
            continue;
        }
        let after = chars.get(index + 1).map(|(_, next)| *next);
        if after.is_none_or(char::is_whitespace) {
            return Some(byte + ch.len_utf8());
        }
    }
    None
}

/// Approvals are handled by the TUI's popup.
///
/// The approver hands the request to the interface and waits for the answer,
/// so the agent pauses until the user has actually decided.
pub struct TuiApprover {
    tx: mpsc::Sender<PendingApproval>,
}

#[async_trait::async_trait]
impl crate::agent::ApprovalHandler for TuiApprover {
    async fn request(&self, request: ApprovalRequest) -> Option<ApprovalDecision> {
        let (pending, answer) = PendingApproval::new(request);
        self.tx.send(pending).await.ok()?;
        answer.await.ok()
    }
}

/// Rebuild a transcript from a session's messages.
///
/// The tools a session used are shown with their results, so a session
/// picked from the list reads the same way it did when it happened.
fn lines_from_messages(messages: &[crate::provider::types::Message]) -> Vec<Line> {
    use crate::provider::types::{ContentBlock, Role};

    let mut lines: Vec<Line> = Vec::new();

    for message in messages {
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    match message.role {
                        Role::User => lines.push(Line::User(text.clone())),
                        Role::Assistant => lines.push(Line::Assistant(text.clone())),
                        _ => {}
                    }
                }
                ContentBlock::Thinking { text } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    lines.push(Line::Thinking(text.clone()));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    lines.push(Line::Tool {
                        id: id.clone(),
                        name: name.clone(),
                        input: serde_json::to_string_pretty(input).unwrap_or_default(),
                        result: None,
                    });
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    // Fill in the result of the tool call this answers.
                    for line in lines.iter_mut() {
                        if let Line::Tool {
                            id: line_id,
                            result,
                            ..
                        } = line
                            && line_id == tool_use_id
                        {
                            *result = Some(ToolResultView {
                                content: content.clone(),
                                is_error: *is_error,
                            });
                            break;
                        }
                    }
                }
                ContentBlock::Image { .. } => {}
            }
        }
    }

    lines
}

/// The list of things to do, as the tool reported it.
fn plan_of(output: &crate::tool::ToolOutput) -> Vec<crate::tool::todo::TodoItem> {
    output
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("todos"))
        .and_then(|todos| serde_json::from_value(todos.clone()).ok())
        .unwrap_or_default()
}
