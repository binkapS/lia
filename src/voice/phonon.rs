//! In-process neural speech synthesis powered by Phonon (`ptts`).
//!
//! Phonon generates speech directly from text using a flow-matching neural
//! model and a Mimi neural audio decoder. Weights are downloaded on first
//! use or read from a local directory, and synthesis runs in-process on the
//! CPU. When the source is an `hf://` URL the `hub` feature downloads the
//! weights from Hugging Face on the first request and caches them locally.
//!
//! A loaded model is shared by every speaker in the process: the gateway,
//! the tools, the terminal and the interface all ask for the same checkpoint,
//! and only one copy is kept in memory and one download is made. Synthesis
//! itself is serialized, because the CPU is doing the work either way.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use ptts::checkpoint::{Checkpoint, DEFAULT_MODEL, ResolveOptions};
use ptts::preprocess::{Lang, Normalize};
use ptts::synth::{DeviceKind, Quant, SpeechOptions, Synth};
use ptts::voice_catalog::{DEFAULT_VOICE_CATALOG, VoiceCatalog};

use super::detect::Player;
use super::speak;

/// How much audio is gathered before it is handed to the player.
///
/// The model produces a chunk every few frames. A player started for each
/// one of those would spend more time starting than playing, so chunks are
/// gathered into a little under a third of a second: soon enough to be heard
/// as streaming, long enough that the player is not restarted constantly.
const PLAY_BATCH_MS: u64 = 320;

/// A loaded model, shared by every speaker that wants the same one.
type SharedSynth = Arc<Mutex<Synth>>;

/// The models loaded in this process, by what makes them different.
///
/// The voice is not part of the key: voices are registered into a model, so
/// two speakers with different voices can share one checkpoint.
fn models() -> &'static Mutex<HashMap<String, SharedSynth>> {
    static MODELS: OnceLock<Mutex<HashMap<String, SharedSynth>>> = OnceLock::new();
    MODELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Take a lock, whatever a panic elsewhere did to it.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The language a text should be normalized in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    /// Normalize in this language.
    Normalized(Lang),
    /// Hand the text to the model as written.
    AsWritten,
}

impl Language {
    /// The language named in the configuration.
    ///
    /// The configuration may name a locale (`en-US`), which is the language
    /// of its first part. `none` and `off` turn normalization off, which is
    /// what text that is already spoken forms wants. A language Phonon does
    /// not speak falls back to English, which is what the default model is
    /// trained on.
    fn from_config(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::Normalized(Lang::En);
        };
        let name = value.to_ascii_lowercase();
        match name.as_str() {
            "none" | "off" => return Self::AsWritten,
            _ => {}
        }
        let code = name.split(['-', '_']).next().unwrap_or_default();
        code.parse::<Lang>()
            .map(Self::Normalized)
            .unwrap_or(Self::Normalized(Lang::En))
    }

    /// How text is handed to the model.
    fn normalize(self) -> Normalize {
        match self {
            Self::Normalized(lang) => Normalize::for_lang(lang),
            Self::AsWritten => Normalize::OFF,
        }
    }

    /// The part of the model cache key this language makes.
    fn key(self) -> &'static str {
        match self {
            Self::Normalized(Lang::En) => "en",
            Self::Normalized(Lang::Fr) => "fr",
            Self::Normalized(Lang::De) => "de",
            Self::Normalized(Lang::Es) => "es",
            Self::Normalized(Lang::Pt) => "pt",
            Self::AsWritten => "none",
        }
    }
}

/// A neural speech synthesizer running in-process via Phonon (`ptts`).
pub struct PhononSpeaker {
    checkpoint_source: String,
    quant: Quant,
    device: DeviceKind,
    language: Language,
    /// The voice the configuration asked for, by name or by voice file.
    voice: Option<String>,
    /// The name the model knows that voice by, once the model is loaded.
    resolved_voice: Mutex<Option<String>>,
    /// The player that makes the sound, when this machine has one.
    player: Option<Player>,
    /// This speaker's handle on the shared model. `None` until first use.
    model: Mutex<Option<SharedSynth>>,
}

impl std::fmt::Debug for PhononSpeaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhononSpeaker")
            .field("checkpoint_source", &self.checkpoint_source)
            .field("quant", &self.quant.as_str())
            .field("device", &self.device)
            .field("language", &self.language)
            .field("voice", &self.voice)
            .field(
                "player",
                &self.player.as_ref().map(|player| &player.program),
            )
            .finish()
    }
}

impl PhononSpeaker {
    /// Create a new Phonon speaker.
    ///
    /// A player is only needed to make a sound; synthesis into a file or an
    /// HTTP response works without one.
    pub fn new(
        model_source: Option<String>,
        lang: Option<&str>,
        voice: Option<String>,
        player: Option<Player>,
    ) -> Self {
        let checkpoint_source = model_source
            .map(|source| source.trim().to_string())
            .filter(|source| !source.is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        let voice = voice
            .map(|voice| voice.trim().to_string())
            .filter(|voice| !voice.is_empty());

        Self {
            checkpoint_source,
            // q8 weights are a third of the size of f32 and run well on a
            // CPU, which is where an assistant on a laptop lives.
            quant: Quant::Q80,
            device: DeviceKind::Cpu,
            language: Language::from_config(lang),
            voice,
            resolved_voice: Mutex::new(None),
            player,
            model: Mutex::new(None),
        }
    }

    /// The name of the voice or model for reporting.
    pub fn model_name(&self) -> String {
        if let Some(rest) = self.checkpoint_source.strip_prefix("hf://") {
            rest.split('@').next().unwrap_or(rest).to_string()
        } else {
            self.checkpoint_source.clone()
        }
    }

    /// The audio player that makes the sound, when there is one.
    pub fn player(&self) -> Option<&Player> {
        self.player.as_ref()
    }

    /// Warm the model up before the first request, so that the first word
    /// is not the one to pay the cost of loading the weights.
    ///
    /// Loading happens once per process; the shared model is reused, so
    /// this only matters for the first speaker that asks for it. Errors
    /// are reported but never turned into a failed voice, since a model
    /// that cannot load is reported again at synthesis time.
    pub fn warm(&self) {
        if let Err(error) = self.shared() {
            tracing::warn!("could not warm the speech model: {error}");
        }
    }

    /// The voices registered with the loaded model, sorted by name.
    ///
    /// The checkpoint's own voices come first, then the voices of the
    /// catalog, so that a voice besides the built-in one can be chosen. The
    /// catalog is read best-effort: offline, the checkpoint's own voices are
    /// still listed.
    pub fn voices(&self) -> Vec<String> {
        let mut voices = self
            .with_model(|model| {
                let tts = lock(model);
                Ok(tts.voices())
            })
            .unwrap_or_else(|error| {
                tracing::warn!("could not load speech model for voice list: {error}");
                Vec::new()
            });

        if let Ok(catalog) = VoiceCatalog::new(DEFAULT_VOICE_CATALOG)
            && let Ok(known) = catalog.voices()
        {
            for voice in known {
                if !voices
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&voice.name))
                {
                    voices.push(voice.name.clone());
                }
            }
        }
        voices.sort();
        voices
    }

    /// Synthesize speech into mono PCM and return the samples and sample rate.
    pub fn synthesize_pcm(&self, text: &str) -> Result<(Vec<f32>, u32)> {
        self.synthesize_pcm_with(text, None)
    }

    /// Synthesize speech in the voice named, or the configured one when
    /// `voice` is `None`.
    pub fn synthesize_pcm_with(&self, text: &str, voice: Option<&str>) -> Result<(Vec<f32>, u32)> {
        self.with_model(|model| {
            let mut tts = lock(model);
            let options = self.options(&mut tts, voice)?;
            let pcm = match &options {
                Some(options) => tts
                    .say_with(text, options)
                    .context("speech synthesis failed")?,
                None => tts.say(text).context("speech synthesis failed")?,
            };
            Ok((pcm, tts.sample_rate()))
        })
    }

    /// Synthesize speech into 16-bit WAV bytes.
    pub fn synthesize_wav(&self, text: &str) -> Result<Vec<u8>> {
        self.synthesize_wav_with(text, None)
    }

    /// Synthesize speech into 16-bit WAV bytes in the voice named.
    pub fn synthesize_wav_with(&self, text: &str, voice: Option<&str>) -> Result<Vec<u8>> {
        let (pcm, sample_rate) = self.synthesize_pcm_with(text, voice)?;
        wav_bytes(&pcm, sample_rate)
    }

    /// Synthesize speech and save directly to a WAV file.
    pub fn synthesize_to_file(&self, text: &str, path: impl AsRef<Path>) -> Result<()> {
        let (pcm, sample_rate) = self.synthesize_pcm(text)?;
        ptts::wav::write_wav_file(path, &pcm, sample_rate).context("could not write wav file")?;
        Ok(())
    }

    /// Synthesize speech and play it, streaming chunk by chunk.
    ///
    /// Phonon's streaming API yields audio chunks as the model decodes them,
    /// so the first sound is heard before the whole utterance has been
    /// synthesized. Chunks are batched into short pieces so that restarting
    /// the player does not swallow the sound between them.
    ///
    /// The model is loaded (once) inside the blocking task. Dropping the
    /// returned future drops the receiver of the audio, which stops the
    /// synthesis and the player at the next chunk.
    pub async fn speak(self: &Arc<Self>, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let player = self
            .player
            .clone()
            .context("no audio player was found, so the speech cannot be heard")?;

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>>>(4);

        let me = Arc::clone(self);
        let text = text.to_string();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = me.stream_wavs(&text, &tx) {
                let _ = tx.blocking_send(Err(error));
            }
        });

        while let Some(wav) = rx.recv().await {
            speak::play(&player, &wav?).await?;
        }

        Ok(())
    }

    /// Load the shared model, if it is not loaded yet, and give it back.
    fn with_model<T>(&self, use_model: impl FnOnce(&SharedSynth) -> Result<T>) -> Result<T> {
        let model = self.shared()?;
        use_model(&model)
    }

    /// The shared model this speaker speaks with.
    fn shared(&self) -> Result<SharedSynth> {
        if let Some(model) = lock(&self.model).as_ref() {
            return Ok(Arc::clone(model));
        }

        let key = self.model_key();
        let model = {
            let mut loaded = lock(models());
            match loaded.get(&key) {
                Some(model) => Arc::clone(model),
                None => {
                    let model = Arc::new(Mutex::new(self.load()?));
                    loaded.insert(key, Arc::clone(&model));
                    model
                }
            }
        };

        // A voice is a property of the speaker, not of the checkpoint: two
        // speakers may share the model and ask for different ones.
        self.ensure_voice(&model)?;
        *lock(&self.model) = Some(Arc::clone(&model));
        Ok(model)
    }

    /// What makes this model different from another one in the cache.
    fn model_key(&self) -> String {
        format!(
            "{}|{}|{}|{:?}",
            self.checkpoint_source,
            self.language.key(),
            self.quant.as_str(),
            self.device
        )
    }

    /// Build the model from its checkpoint.
    fn load(&self) -> Result<Synth> {
        let checkpoint = Checkpoint::locate(
            &self.checkpoint_source,
            ResolveOptions {
                quant: self.quant,
                weights: None,
            },
        )
        .with_context(|| {
            format!(
                "could not locate the speech model at {}",
                self.checkpoint_source
            )
        })?;

        let mut tts = checkpoint
            .builder(self.language.normalize())
            .device(self.device)
            .build()
            .context("could not build the speech synthesizer")?;

        checkpoint.register_voices(&mut tts);

        Ok(tts)
    }

    /// Make sure the configured voice is known to the model, downloading it
    /// from the catalog when the checkpoint does not have it.
    fn ensure_voice(&self, model: &SharedSynth) -> Result<()> {
        if lock(&self.resolved_voice).is_some() {
            return Ok(());
        }
        let Some(query) = self.voice.as_deref() else {
            return Ok(());
        };

        let mut tts = lock(model);
        let name = register_voice(&mut tts, query)?;
        *lock(&self.resolved_voice) = Some(name);
        Ok(())
    }

    /// The options for one request: the voice it names, else the configured
    /// one, else the model's own default.
    fn options(&self, tts: &mut Synth, requested: Option<&str>) -> Result<Option<SpeechOptions>> {
        let requested = requested.map(str::trim).filter(|voice| !voice.is_empty());

        if let Some(voice) = requested {
            let name = register_voice(tts, voice)?;
            return Ok(Some(SpeechOptions::default().voice(name)));
        }

        match lock(&self.resolved_voice).clone() {
            Some(voice) => Ok(Some(SpeechOptions::default().voice(voice))),
            None => Ok(None),
        }
    }

    /// Stream the words as WAV bytes, batching the chunks for the player.
    fn stream_wavs(
        &self,
        text: &str,
        tx: &tokio::sync::mpsc::Sender<Result<Vec<u8>>>,
    ) -> Result<()> {
        let model = self.shared()?;
        let mut tts = lock(&model);

        let options = self.options(&mut tts, None)?;
        let stream = match &options {
            Some(options) => tts.stream_with(text, options),
            None => tts.stream(text),
        }
        .context("speech synthesis could not start")?;

        let sample_rate = stream.sample_rate();
        let batch = (sample_rate as u64 * PLAY_BATCH_MS / 1000) as usize;
        let mut pending: Vec<f32> = Vec::new();

        for chunk in stream {
            let pcm = chunk.context("speech synthesis failed")?;
            pending.extend_from_slice(&pcm);

            if pending.len() >= batch.max(1) {
                if !send_wav(tx, &pending, sample_rate)? {
                    return Ok(());
                }
                pending.clear();
            }
        }

        if !pending.is_empty() {
            send_wav(tx, &pending, sample_rate)?;
        }
        Ok(())
    }
}

/// Send one piece of audio, saying whether the listener is still there.
fn send_wav(
    tx: &tokio::sync::mpsc::Sender<Result<Vec<u8>>>,
    pcm: &[f32],
    sample_rate: u32,
) -> Result<bool> {
    let wav = wav_bytes(pcm, sample_rate)?;
    Ok(tx.blocking_send(Ok(wav)).is_ok())
}

/// Wrap mono PCM in a WAV file.
fn wav_bytes(pcm: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let mut wav = Vec::new();
    ptts::wav::write_pcm_as_wav(&mut wav, pcm, sample_rate, 1)
        .context("could not encode the wav audio")?;
    Ok(wav)
}

/// Find the voice a request names, or register it from a file or the catalog.
///
/// A name the model already has is used as it is, matched without regard to
/// case so that `alba` finds the catalog's `Alba`. A path to a voice file
/// registers that file under its own name. Anything else is looked up in the
/// voice catalog and downloaded only when it is first used.
fn register_voice(tts: &mut Synth, query: &str) -> Result<String> {
    let query = query.trim();
    if query.is_empty() {
        bail!("the voice name is empty");
    }

    let known = tts.voices();
    if let Some(name) = known.iter().find(|name| name.eq_ignore_ascii_case(query)) {
        return Ok(name.clone());
    }

    let path = Path::new(query);
    if path.is_file() {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .ok_or_else(|| anyhow!("the voice file {} has no name", path.display()))?
            .to_string();
        if !known.iter().any(|known| known == &name) {
            tts.add_voice_file(&name, path)
                .with_context(|| format!("could not read the voice file {}", path.display()))?;
        }
        return Ok(name);
    }

    let catalog = VoiceCatalog::new(DEFAULT_VOICE_CATALOG)
        .context("the voice catalog could not be opened")?;
    let name = catalog.register(tts, query).with_context(|| {
        format!(
            "no voice '{query}'; the model has {}",
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        )
    })?;

    // The catalog answers with the name unchanged when it does not know it,
    // which the model then refuses at synthesis time. Say so here, where the
    // names the model does have can be printed.
    if tts.voices().iter().any(|known| known == &name) {
        Ok(name)
    } else {
        bail!(
            "no voice '{query}'; the model has {}",
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locale_is_normalized_by_its_language() {
        assert_eq!(
            Language::from_config(Some("en-US")),
            Language::Normalized(Lang::En)
        );
        assert_eq!(
            Language::from_config(Some("fr_CA")),
            Language::Normalized(Lang::Fr)
        );
        assert_eq!(
            Language::from_config(Some("PT-br")),
            Language::Normalized(Lang::Pt)
        );
    }

    #[test]
    fn an_unnamed_language_is_english() {
        assert_eq!(Language::from_config(None), Language::Normalized(Lang::En));
        assert_eq!(
            Language::from_config(Some("  ")),
            Language::Normalized(Lang::En)
        );
    }

    #[test]
    fn normalization_can_be_turned_off() {
        let language = Language::from_config(Some("none"));
        assert_eq!(language, Language::AsWritten);
        assert_eq!(language.normalize(), Normalize::OFF);
    }

    #[test]
    fn a_language_phonon_does_not_speak_falls_back_to_english() {
        assert_eq!(
            Language::from_config(Some("ja")),
            Language::Normalized(Lang::En)
        );
    }

    #[test]
    fn a_hugging_face_model_is_named_without_its_scheme_or_revision() {
        let speaker = PhononSpeaker::new(
            Some("hf://gradium/phonon-31m-en@abc123".into()),
            Some("en"),
            None,
            None,
        );
        assert_eq!(speaker.model_name(), "gradium/phonon-31m-en");
    }

    #[test]
    fn a_local_model_keeps_its_path() {
        let speaker = PhononSpeaker::new(Some("/models/phonon".into()), Some("en"), None, None);
        assert_eq!(speaker.model_name(), "/models/phonon");
    }

    #[test]
    fn a_speaker_without_a_player_can_still_synthesize() {
        let speaker = PhononSpeaker::new(None, None, None, None);
        assert!(speaker.player().is_none());
    }

    #[test]
    fn the_model_cache_key_separates_languages() {
        let english = PhononSpeaker::new(None, Some("en"), None, None);
        let french = PhononSpeaker::new(None, Some("fr"), None, None);
        assert_ne!(english.model_key(), french.model_key());
    }
}
