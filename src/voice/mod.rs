//! Listening and speaking.
//!
//! lia can hear and speak. Hearing uses programs on the machine: the sound
//! is recorded by one and turned into words by another. Speaking has a voice
//! built in — Phonon, a small neural text-to-speech model that runs in this
//! process — and falls back to a program or an endpoint when that is wanted.
//! The programs are looked for on the `PATH` and can be named in the
//! configuration, so the same code works with whisper.cpp here and with an
//! OpenAI-compatible endpoint there.
//!
//! The pieces are:
//!
//! - [`detect`] — what is on this machine.
//! - [`compose`] — the words the voice says, composed like a person's.
//! - [`phonon`] — the neural voice that runs in this process.
//! - [`record`] — the microphone, and the level of the sound coming in.
//! - [`transcribe`] — words from sound.
//! - [`speak`] — sound from words.
//!
//! On top of them sits [`Voice`], which the rest of lia uses, and
//! [`VoiceWorker`], which lets the interface ask for a recording without
//! waiting for it.

pub mod compose;
pub mod detect;
pub mod phonon;
pub mod record;
pub mod speak;
pub mod transcribe;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinSet;

use crate::config::VoiceConfig;

pub use detect::Capabilities;
pub use record::{RecordOptions, Recorder, Recording};
pub use speak::Speaker;
pub use transcribe::Transcriber;

/// How often the level of the sound is reported, at most.
///
/// Twenty times a second is more than the eye can follow, and few enough
/// that the interface is never late for a key.
const REPORT_EVERY: Duration = Duration::from_millis(50);

/// The speaker the configuration asks for.
///
/// A command or an endpoint named outright wins. Then Phonon, when it is
/// named as the provider. Otherwise the machine is looked over: a neural
/// engine that writes a wav (piper) is preferred, then the Phonon runtime
/// that is built into lia, and only then a program that reads the words out
/// itself.
fn speaker_from(config: &VoiceConfig, player: &Option<detect::Player>) -> Option<Speaker> {
    // When MODEL_DIR is set, Phonon speaks with that checkpoint, ahead of
    // anything named in the configuration.
    if let Ok(source) = std::env::var("MODEL_DIR")
        && !source.trim().is_empty()
    {
        return Some(Speaker::phonon(
            Some(source),
            config.language.as_deref(),
            config.voice.clone(),
            player.clone(),
        ));
    }

    if let Some(command) = config.speak_command.as_ref().filter(|c| !c.is_empty()) {
        let (program, args) = command.split_first().expect("the command is not empty");
        return Some(Speaker::Command {
            program: program.clone(),
            args: args.to_vec(),
        });
    }

    if let Some(url) = &config.speak_url {
        // How the endpoint is spoken to: it may be named, and when it is
        // not, its address is looked at — one that names elevenlabs is one
        // of its kind.
        let api = speak::ApiStyle::named(config.speak_provider.as_deref(), url);
        let model = config
            .speak_api_model
            .clone()
            .unwrap_or_else(|| api.default_model().to_string());
        // The key is read from the variable the configuration names; when it
        // does not name one, the variable each kind of endpoint has of its
        // own is read.
        let key = match &config.speak_api_key_env {
            Some(name) => crate::secrets::var(name),
            None => api.default_key_env().and_then(crate::secrets::var),
        };
        return player.clone().map(|player| Speaker::Http {
            api,
            url: url.clone(),
            key,
            model,
            voice: config.voice.clone(),
            player,
        });
    }

    // Phonon named as the provider does not need a player to synthesize:
    // the gateway can serve the audio even on a machine with no sound card.
    if names_phonon(config) {
        return Some(Speaker::phonon(
            phonon_source(config, true),
            config.language.as_deref(),
            config.voice.clone(),
            player.clone(),
        ));
    }

    let options = config.speak_options.clone().unwrap_or_default();

    if let Some(command) = config.speak_wav_command.as_ref().filter(|c| !c.is_empty())
        && let Some(player) = player.clone()
    {
        let (program, args) = command.split_first().expect("the command is not empty");
        return Some(Speaker::WavCommand {
            program: program.clone(),
            args: args.to_vec(),
            player,
        });
    }

    // A model named in `speak_model` that is a Phonon checkpoint is not
    // handed to piper: the checkpoint was named for the built-in voice.
    if let Some(source) = phonon_source(config, false) {
        return match player.clone() {
            Some(player) => Some(Speaker::phonon(
                Some(source),
                config.language.as_deref(),
                config.voice.clone(),
                Some(player),
            )),
            // Without a player the checkpoint cannot be heard, and piper
            // cannot read it either: only a program that speaks by itself
            // can.
            None => {
                detect::speak_command().map(|(program, args)| Speaker::Command { program, args })
            }
        };
    }

    // A neural voice that writes a wav is preferred, when there is a player
    // to hear it with.
    if let Some((program, args)) =
        detect::speak_wav_command(config.speak_model.as_deref(), &options)
        && let Some(player) = player.clone()
    {
        return Some(Speaker::WavCommand {
            program,
            args,
            player,
        });
    }

    // Phonon is built into lia, so a player is enough to have a voice that
    // sounds like a person rather than like a machine.
    if let Some(player) = player.clone() {
        return Some(Speaker::phonon(
            None,
            config.language.as_deref(),
            config.voice.clone(),
            Some(player),
        ));
    }

    detect::speak_command().map(|(program, args)| Speaker::Command { program, args })
}

/// Whether the configuration names Phonon as the speech provider.
fn names_phonon(config: &VoiceConfig) -> bool {
    config.speak_provider.as_deref().is_some_and(|provider| {
        matches!(
            provider.trim().to_ascii_lowercase().as_str(),
            "phonon" | "ptts"
        )
    })
}

/// The checkpoint Phonon should load, when one is named for it.
///
/// `speak_model` is shared with piper, whose model is a file rather than a
/// checkpoint. When Phonon was chosen by name, whatever is named is taken as
/// its checkpoint. When it was chosen because nothing else was found, only a
/// value that can be a checkpoint is taken: an `hf://` repo, a directory, or
/// a `config.json`.
fn phonon_source(config: &VoiceConfig, explicit: bool) -> Option<String> {
    let source = config
        .speak_model
        .as_ref()?
        .to_string_lossy()
        .trim()
        .to_string();
    if source.is_empty() {
        return None;
    }
    if explicit {
        return Some(source);
    }
    let looks_like_a_checkpoint = source.starts_with("hf://")
        || Path::new(&source).is_dir()
        || Path::new(&source)
            .file_name()
            .is_some_and(|name| name == "config.json");
    looks_like_a_checkpoint.then_some(source)
}

/// The voice, as the rest of lia uses it.
///
/// It is cheap to build and safe to share: the microphone is guarded, so a
/// recording started by the model and one started by the person in the
/// terminal cannot both have it.
#[derive(Debug)]
pub struct Voice {
    config: VoiceConfig,
    recorder: Option<Recorder>,
    transcriber: Option<Transcriber>,
    speaker: Option<Speaker>,
    /// The microphone. Only one thing records at a time.
    microphone: Mutex<()>,
}

impl Voice {
    /// Build the voice from a configuration.
    pub fn new(config: VoiceConfig) -> Self {
        let recorder = match &config.record_command {
            Some(command) if !command.is_empty() => Some(Recorder::new(command.clone())),
            _ => detect::record_command().map(|(program, args)| {
                let mut command = vec![program];
                command.extend(args);
                Recorder::new(command)
            }),
        };

        let transcriber = match &config.transcribe_command {
            Some(command) if !command.is_empty() => {
                let (program, args) = command.split_first().expect("the command is not empty");
                Some(Transcriber::Command {
                    program: program.clone(),
                    args: args.to_vec(),
                })
            }
            _ => match &config.transcribe_url {
                Some(url) => Some(Transcriber::Http {
                    url: url.clone(),
                    key: config
                        .transcribe_api_key_env
                        .as_deref()
                        .and_then(crate::secrets::var),
                    model: config
                        .transcribe_api_model
                        .clone()
                        .unwrap_or_else(|| "whisper-1".to_string()),
                    language: config.language.clone(),
                }),
                None => detect::transcribe_command(config.transcribe_model.as_deref())
                    .map(|(program, args)| Transcriber::Command { program, args }),
            },
        };

        let player = match &config.play_command {
            Some(command) if !command.is_empty() => {
                let (program, args) = command.split_first().expect("the command is not empty");
                let mut all = vec![program.clone()];
                all.extend(args.iter().cloned());
                Some(detect::Player {
                    program: program.clone(),
                    command: all,
                })
            }
            _ => detect::player(),
        };

        let speaker = speaker_from(&config, &player);

        if let Some(Speaker::Phonon { speaker }) = &speaker {
            let speaker = speaker.clone();
            std::thread::spawn(move || speaker.warm());
        }

        Voice {
            config,
            recorder,
            transcriber,
            speaker,
            microphone: Mutex::new(()),
        }
    }

    /// The configuration this voice was built from.
    pub fn config(&self) -> &VoiceConfig {
        &self.config
    }

    /// What this voice can do.
    ///
    /// Only what is really there is reported: a program that is named but
    /// not installed is not a recorder.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            recorder: self
                .recorder
                .as_ref()
                .filter(|r| recorder_exists(r))
                .map(|r| r.command()[0].clone()),
            transcriber: self
                .transcriber
                .as_ref()
                .filter(|t| t.exists())
                .map(|t| t.describe()),
            speaker: self
                .speaker
                .as_ref()
                .filter(|s| s.exists() || s.can_synthesize())
                .map(|s| s.describe()),
            voice_model: self.speaker.as_ref().and_then(|s| s.model_name()),
            player: None,
        }
    }

    /// Whether lia can hear.
    ///
    /// Both the recorder and the transcriber have to be there, not merely
    /// named: a program that is missing is no recorder at all.
    pub fn can_listen(&self) -> bool {
        self.recorder.as_ref().is_some_and(recorder_exists)
            && self.transcriber.as_ref().is_some_and(|t| t.exists())
    }

    /// Whether lia can speak.
    pub fn can_speak(&self) -> bool {
        self.speaker.as_ref().is_some_and(|s| s.exists())
    }

    /// Whether words can be turned into sound, even if this machine cannot
    /// play it.
    ///
    /// The gateway uses this: it can serve a wav to a browser on a machine
    /// with no sound card, where [`Self::can_speak`] is false.
    pub fn can_synthesize(&self) -> bool {
        self.speaker
            .as_ref()
            .is_some_and(|speaker| speaker.can_synthesize())
    }

    /// Listen, and give back the words.
    pub async fn listen(&self, options: RecordOptions) -> Result<String> {
        // The sender is kept for as long as the recording lasts: a watch
        // that has no sender ends the recording at once.
        let (stop, _seen) = tokio::sync::watch::channel(false);
        self.listen_with(options, stop, |_| {}).await
    }

    /// Listen, and give back the words, reporting the level as it goes.
    ///
    /// The recording is stopped when `stop` is set to true, and what was
    /// said is still turned into words. The sender has to be kept alive for
    /// as long as the recording lasts: when it is dropped, the recording
    /// ends at once, and the sound is lost.
    pub async fn listen_with(
        &self,
        options: RecordOptions,
        stop: tokio::sync::watch::Sender<bool>,
        on_level: impl FnMut(f32) + Send,
    ) -> Result<String> {
        self.listen_cancellable(options, stop, None, on_level).await
    }

    /// Listen, and give back the words, unless the sound is thrown away.
    ///
    /// When `discard` is set while the recording is going on, the sound is
    /// not turned into words at all: the recording stops, the file is
    /// removed, and the words are an empty string. This is what a cancelled
    /// recording needs, so that the microphone is free again at once rather
    /// than after a transcription nobody asked for.
    pub async fn listen_cancellable(
        &self,
        options: RecordOptions,
        stop: tokio::sync::watch::Sender<bool>,
        discard: Option<&AtomicBool>,
        on_level: impl FnMut(f32) + Send,
    ) -> Result<String> {
        let recorder = self
            .recorder
            .as_ref()
            .context("nothing on this machine can record the microphone")?;
        let transcriber = self
            .transcriber
            .as_ref()
            .context("nothing on this machine can turn sound into words")?;

        // Only one recording at a time: the microphone is a single thing.
        let _guard = self.microphone.lock().await;

        // The recording is stopped by setting the watch, so the child is
        // killed by its own pid and never by a pattern.
        let recording = recorder
            .record(options, stop.subscribe(), on_level)
            .await
            .context("the microphone could not be recorded")?;

        // The sound was thrown away while it was still coming in.
        if discard.is_some_and(|discard| discard.load(Ordering::SeqCst)) {
            let _ = std::fs::remove_file(&recording.path);
            return Ok(String::new());
        }

        if recording.seconds < 0.2 {
            return Ok(String::new());
        }

        let text = transcriber
            .transcribe(&recording.path)
            .await
            .context("the sound could not be turned into words")?;

        // The sound has served its purpose.
        let _ = std::fs::remove_file(&recording.path);
        Ok(text.trim().to_string())
    }

    /// Say the words.
    ///
    /// The words are turned into prose first: the marks that make writing
    /// readable are of no use to a voice.
    pub async fn say(&self, text: &str) -> Result<()> {
        let speaker = self
            .speaker
            .as_ref()
            .context("nothing on this machine can speak")?;
        speaker.speak(&speak::plain(text)).await
    }

    /// Synthesize speech into mono PCM samples and sample rate using Phonon.
    pub fn synthesize(&self, text: &str) -> Result<(Vec<f32>, u32)> {
        self.synthesize_with(text, None)
    }

    /// Synthesize speech into mono PCM, in the voice named or the configured
    /// one when `voice` is `None`.
    pub fn synthesize_with(&self, text: &str, voice: Option<&str>) -> Result<(Vec<f32>, u32)> {
        let text = speak::plain(text);
        match &self.speaker {
            Some(Speaker::Phonon { speaker }) => speaker.synthesize_pcm_with(&text, voice),
            _ => anyhow::bail!("Phonon speech model is not active"),
        }
    }

    /// Synthesize speech into 16-bit WAV bytes using Phonon.
    pub fn synthesize_wav(&self, text: &str) -> Result<Vec<u8>> {
        self.synthesize_wav_with(text, None)
    }

    /// Synthesize speech into 16-bit WAV bytes, in the voice named or the
    /// configured one when `voice` is `None`.
    pub fn synthesize_wav_with(&self, text: &str, voice: Option<&str>) -> Result<Vec<u8>> {
        let text = speak::plain(text);
        match &self.speaker {
            Some(Speaker::Phonon { speaker }) => speaker.synthesize_wav_with(&text, voice),
            _ => anyhow::bail!("Phonon speech model is not active"),
        }
    }

    /// Turn the words into WAV bytes for a client that will play them.
    ///
    /// This is the gateway's path: it works for the built-in Phonon voice and
    /// for a program such as piper that writes a wav, so the browser hears
    /// the same voice as the terminal. Nothing is played here, which is what
    /// makes it useful on a machine with no sound card.
    pub async fn synthesize_wav_async(&self, text: &str, voice: Option<&str>) -> Result<Vec<u8>> {
        let text = speak::plain(text);
        let voice = voice.map(str::to_string);
        match &self.speaker {
            Some(Speaker::Phonon { speaker }) => {
                let speaker = speaker.clone();
                tokio::task::spawn_blocking(move || {
                    speaker.synthesize_wav_with(&text, voice.as_deref())
                })
                .await
                .context("the speech task ended")?
            }
            Some(speaker) => speaker.wav(&text).await,
            None => anyhow::bail!("nothing on this machine can speak"),
        }
    }

    /// Whether [`Self::synthesize`] can make raw PCM.
    ///
    /// Only the built-in Phonon voice can; a program that writes a wav is
    /// served as a wav.
    pub fn can_synthesize_pcm(&self) -> bool {
        matches!(self.speaker, Some(Speaker::Phonon { .. }))
    }

    /// List the voices the active Phonon model offers.
    ///
    /// The checkpoint's own voices come first, then the voice catalog's, so
    /// that a voice besides the built-in one can be chosen. Loads the model
    /// on first call if it is not already loaded. Returns an empty list when
    /// Phonon is not the active speaker. This is the data behind the
    /// `/v1/audio/voices` gateway endpoint.
    pub fn phonon_voices(&self) -> Vec<String> {
        match &self.speaker {
            Some(Speaker::Phonon { speaker }) => speaker.voices(),
            _ => Vec::new(),
        }
    }

    /// Write mono PCM samples to a WAV file using Phonon.
    pub fn write_wav_file<P: AsRef<std::path::Path>>(
        &self,
        path: P,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<()> {
        ptts::wav::write_wav_file(path, pcm, sample_rate).map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// The word lia answers to.
pub const WAKE_WORD: &str = "lia";

/// Take the message out of what was said, if it was said to lia.
///
/// The name is taken away, along with the marks around it, so that "Hey
/// lia, what is the weather?" becomes "what is the weather?". Nothing is
/// returned when the words were not meant for lia at all, which is what
/// the hands-free mode needs: the room is full of talk, and only some of
/// it is for her.
///
/// The name of lia in the middle of a sentence is left where it is: the
/// words before it are part of the message.
pub fn addressed_to(text: &str, name: &str) -> Option<String> {
    let mut before = String::new();
    let mut after = String::new();
    let mut found = false;

    for word in text.split_whitespace() {
        // The name is a word of its own: it is not part of a longer one.
        let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
        if !found && bare.eq_ignore_ascii_case(name) {
            found = true;
            continue;
        }
        if found {
            if !after.is_empty() {
                after.push(' ');
            }
            after.push_str(word);
        } else {
            if !before.is_empty() {
                before.push(' ');
            }
            before.push_str(word);
        }
    }

    if !found {
        return None;
    }

    // What was said after the name is the message.
    let after = trim_marks(&after);
    if !after.is_empty() {
        return Some(after);
    }
    // Only the name was said, so what came before it is the message.
    Some(trim_marks(&before))
}

/// Take the marks that surround a piece of text away.
fn trim_marks(text: &str) -> String {
    text.trim()
        .trim_start_matches([' ', '\t', ',', ':', ';', '-', '—', '–', '.'])
        .trim()
        .to_string()
}

/// Whether a recorder is really there.
fn recorder_exists(recorder: &Recorder) -> bool {
    recorder
        .command()
        .first()
        .is_some_and(|program| detect::which(program).is_some())
}

/// What the interface asks the voice to do.
#[derive(Debug, Clone)]
pub enum VoiceCommand {
    /// Start recording, and turn the sound into words when it ends.
    Listen {
        /// How long the recording may last.
        max_seconds: u64,
        /// How long a silence ends it, if it should end by itself.
        silence_ms: Option<u64>,
    },
    /// Stop the recording that is going on, and keep what was said.
    Stop,
    /// Stop the recording that is going on, and throw the words away.
    Cancel,
    /// Say the words.
    Speak(String),
    /// Stop the words that are being said, and forget the rest.
    StopSpeaking,
    /// Stop everything.
    Quit,
}

/// What the voice reports back.
#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// The microphone is open, and this is how loud it is.
    Listening {
        /// The level, 0..=1.
        level: f32,
        /// How long the recording has been going, in seconds.
        elapsed: f64,
    },
    /// The recording has ended, and these are the words.
    Transcript {
        /// What was said.
        text: String,
    },
    /// The words have been said.
    Spoken,
    /// The words were stopped before they were finished.
    Interrupted,
    /// Something went wrong.
    Failed(String),
}

/// The microphone and the speaker, driven by commands.
///
/// The interface holds one of these and sends it commands; the voice works
/// on its own tasks, so the interface never waits for a recording, and the
/// microphone can stay open while the speaker is talking. That is what live
/// mode needs: the person can talk over the answer.
pub struct VoiceWorker {
    /// Where the commands go.
    pub commands: mpsc::Sender<VoiceCommand>,
    /// Where the events come from.
    pub events: mpsc::Receiver<VoiceEvent>,
}

/// What one of the worker's tasks finished with.
enum WorkerDone {
    /// A recording ended. `Ok(None)` means the sound was thrown away.
    Recorded(Result<Option<String>>),
    /// The speaker stopped, and why.
    Spoke(Spoken),
}

/// How a speaking task ended.
enum Spoken {
    Said,
    Interrupted,
    Failed(String),
}

/// The recording that is going on, and how to stop it.
struct OpenRecording {
    stop: watch::Sender<bool>,
    discard: Arc<AtomicBool>,
}

impl VoiceWorker {
    /// Start the worker.
    pub fn start(voice: Arc<Voice>) -> VoiceWorker {
        let (commands, mut incoming) = mpsc::channel::<VoiceCommand>(16);
        let (outgoing, events) = mpsc::channel::<VoiceEvent>(256);

        tokio::spawn(async move {
            let mut tasks: JoinSet<WorkerDone> = JoinSet::new();
            let mut speech: VecDeque<String> = VecDeque::new();
            let mut recording: Option<OpenRecording> = None;
            // A recording asked for while the microphone was still being
            // freed. It starts as soon as the old one has ended.
            let mut waiting: Option<(u64, Option<u64>)> = None;
            let mut speaking: Option<watch::Sender<bool>> = None;

            loop {
                tokio::select! {
                    command = incoming.recv() => {
                        let Some(command) = command else {
                            // The interface is gone. The sound goes with it.
                            stop_recording(&recording, true);
                            if let Some(stop) = &speaking {
                                let _ = stop.send(true);
                            }
                            break;
                        };

                        match command {
                            VoiceCommand::Listen {
                                max_seconds,
                                silence_ms,
                            } => {
                                if recording.is_some() {
                                    // The microphone is a single thing: the
                                    // request waits for the one in use.
                                    waiting = Some((max_seconds, silence_ms));
                                    continue;
                                }
                                recording = start_recording(
                                    &mut tasks,
                                    &voice,
                                    &outgoing,
                                    max_seconds,
                                    silence_ms,
                                );
                            }
                            VoiceCommand::Stop => stop_recording(&recording, false),
                            VoiceCommand::Cancel => stop_recording(&recording, true),
                            VoiceCommand::Speak(text) => {
                                speech.push_back(text);
                                start_speaking(
                                    &mut speaking,
                                    &mut tasks,
                                    &mut speech,
                                    &voice,
                                );
                            }
                            VoiceCommand::StopSpeaking => {
                                speech.clear();
                                if let Some(stop) = &speaking {
                                    let _ = stop.send(true);
                                }
                            }
                            VoiceCommand::Quit => {
                                stop_recording(&recording, true);
                                if let Some(stop) = &speaking {
                                    let _ = stop.send(true);
                                }
                                break;
                            }
                        }
                    }

                    Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                        match done {
                            Ok(WorkerDone::Recorded(result)) => {
                                recording = None;
                                let event = match result {
                                    Ok(Some(text)) => VoiceEvent::Transcript { text },
                                    // The sound was thrown away, but the
                                    // recording still has to end for the
                                    // interface, which is waiting for it.
                                    Ok(None) => VoiceEvent::Transcript {
                                        text: String::new(),
                                    },
                                    Err(e) => VoiceEvent::Failed(e.to_string()),
                                };
                                if outgoing.send(event).await.is_err() {
                                    break;
                                }
                                // A recording asked for while this one was
                                // still going starts now, after the words
                                // of the last one have been reported.
                                if let Some((max_seconds, silence_ms)) = waiting.take() {
                                    recording = start_recording(
                                        &mut tasks,
                                        &voice,
                                        &outgoing,
                                        max_seconds,
                                        silence_ms,
                                    );
                                }
                            }
                            Ok(WorkerDone::Spoke(spoken)) => {
                                speaking = None;
                                let event = match spoken {
                                    Spoken::Said => VoiceEvent::Spoken,
                                    Spoken::Interrupted => VoiceEvent::Interrupted,
                                    Spoken::Failed(message) => VoiceEvent::Failed(message),
                                };
                                if outgoing.send(event).await.is_err() {
                                    break;
                                }
                                start_speaking(
                                    &mut speaking,
                                    &mut tasks,
                                    &mut speech,
                                    &voice,
                                );
                            }
                            Err(e) => {
                                // A task that panicked must not leave the
                                // interface waiting for it forever.
                                speaking = None;
                                recording = None;
                                if outgoing.send(VoiceEvent::Failed(e.to_string())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });

        VoiceWorker { commands, events }
    }

    /// Ask the voice to do something.
    pub async fn send(&self, command: VoiceCommand) {
        let _ = self.commands.send(command).await;
    }

    /// Stop the recording that is going on.
    pub async fn stop(&self) {
        let _ = self.commands.send(VoiceCommand::Stop).await;
    }

    /// Stop the words that are being said.
    pub async fn stop_speaking(&self) {
        let _ = self.commands.send(VoiceCommand::StopSpeaking).await;
    }

    /// The events, as they arrive.
    pub async fn recv(&mut self) -> Option<VoiceEvent> {
        self.events.recv().await
    }
}

/// Stop the recording that is going on, keeping or throwing away the words.
fn stop_recording(recording: &Option<OpenRecording>, discard: bool) {
    if let Some(recording) = recording {
        if discard {
            recording.discard.store(true, Ordering::SeqCst);
        }
        let _ = recording.stop.send(true);
    }
}

/// Start recording, and give the microphone back when it ends.
fn start_recording(
    tasks: &mut JoinSet<WorkerDone>,
    voice: &Arc<Voice>,
    outgoing: &mpsc::Sender<VoiceEvent>,
    max_seconds: u64,
    silence_ms: Option<u64>,
) -> Option<OpenRecording> {
    let options = RecordOptions {
        max_seconds,
        silence: silence_ms.map(Duration::from_millis),
    };
    let (stop, _seen) = watch::channel(false);
    let discard = Arc::new(AtomicBool::new(false));
    tasks.spawn(record(
        voice.clone(),
        options,
        stop.clone(),
        discard.clone(),
        outgoing.clone(),
    ));
    Some(OpenRecording { stop, discard })
}

/// Give the speaker the next chunk, when it is free.
fn start_speaking(
    speaking: &mut Option<watch::Sender<bool>>,
    tasks: &mut JoinSet<WorkerDone>,
    speech: &mut VecDeque<String>,
    voice: &Arc<Voice>,
) {
    if speaking.is_some() {
        return;
    }
    let Some(text) = speech.pop_front() else {
        return;
    };
    let (stop, seen) = watch::channel(false);
    *speaking = Some(stop);
    tasks.spawn(speak(voice.clone(), text, seen));
}

/// Record, and report the words.
async fn record(
    voice: Arc<Voice>,
    options: RecordOptions,
    stop: watch::Sender<bool>,
    discard: Arc<AtomicBool>,
    outgoing: mpsc::Sender<VoiceEvent>,
) -> WorkerDone {
    let start = Instant::now();

    // The level is reported from inside the recording, so the interface can
    // draw the meter while the sound is still coming in.
    let (level_tx, mut level_rx) = mpsc::unbounded_channel::<f32>();
    let reports = outgoing.clone();
    let levels = tokio::spawn(async move {
        // The level is reported twenty times a second at most. A recorder
        // that fills the pipe faster than the sound really happens would
        // otherwise bury the interface in reports, and the keys would be
        // late.
        let mut last = Instant::now();
        while let Some(level) = level_rx.recv().await {
            if last.elapsed() < REPORT_EVERY {
                continue;
            }
            last = Instant::now();
            let elapsed = start.elapsed().as_secs_f64();
            if reports
                .send(VoiceEvent::Listening { level, elapsed })
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let result = voice
        .listen_cancellable(options, stop, Some(&discard), |level| {
            let _ = level_tx.send(level);
        })
        .await;

    // The recording is over, so the level reporting can stop too.
    drop(level_tx);
    let _ = levels.await;

    if discard.load(Ordering::SeqCst) {
        return WorkerDone::Recorded(Ok(None));
    }
    WorkerDone::Recorded(result.map(Some))
}

/// Say the words, unless they are stopped first.
async fn speak(voice: Arc<Voice>, text: String, mut stop: watch::Receiver<bool>) -> WorkerDone {
    let speaking = voice.say(&text);
    tokio::pin!(speaking);

    let outcome = tokio::select! {
        result = &mut speaking => match result {
            Ok(()) => Spoken::Said,
            Err(e) => Spoken::Failed(e.to_string()),
        },
        _ = stop.changed() => Spoken::Interrupted,
    };

    WorkerDone::Spoke(outcome)
}
