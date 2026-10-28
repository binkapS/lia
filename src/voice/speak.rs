//! Sound from words.
//!
//! Three ways of doing it: the neural voice built into lia (Phonon, which
//! runs in this process), a program that reads the words on its standard
//! input and says them — `spd-say` in its pipe mode, which is what is used
//! on some machines — and an endpoint that answers with audio, which is then
//! played. A program can also turn the words into a wav that a player plays.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::detect::{self, Player};

/// How long speaking may take.
const TIMEOUT: Duration = Duration::from_secs(300);

/// How an endpoint wants to be spoken to.
///
/// Most endpoints that answer with sound speak the same shape: the model
/// and the words in a body, the key as a bearer beside it. ElevenLabs
/// speaks another way: the voice is part of the address, and the key
/// rides in a header of its own. J.A.R.V.I.S's own TTS module is an
/// ElevenLabs voice — a model of `eleven_turbo_v2`, keyed by an
/// `ELEVENLABS_API_KEY` — so the shape is kept, that its voice may be
/// asked the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiStyle {
    /// The shape the OpenAI speech endpoint speaks, and most copies of it.
    OpenAi,
    /// The shape the ElevenLabs speech endpoint speaks.
    ElevenLabs,
}

impl ApiStyle {
    /// Which shape an endpoint wants, by what it is called or named.
    ///
    /// The configuration may name the shape; when it does not, the address
    /// is looked at, and one that names elevenlabs is believed to be one.
    pub fn named(provider: Option<&str>, url: &str) -> Self {
        if let Some(provider) = provider {
            let provider = provider.to_ascii_lowercase().replace(['-', '_', ' '], "");
            match provider.as_str() {
                "elevenlabs" | "eleven" => return Self::ElevenLabs,
                "openai" => return Self::OpenAi,
                _ => {}
            }
        }
        if url.to_ascii_lowercase().contains("elevenlabs") {
            return Self::ElevenLabs;
        }
        Self::OpenAi
    }

    /// The model an endpoint uses when none is named.
    ///
    /// ElevenLabs gets the model J.A.R.V.I.S speaks with, so its voice is
    /// heard the same way.
    pub fn default_model(&self) -> &'static str {
        match self {
            Self::OpenAi => "tts-1",
            Self::ElevenLabs => "eleven_turbo_v2",
        }
    }

    /// The variable a key is read from, when none is named.
    ///
    /// ElevenLabs's own module read `ELEVENLABS_API_KEY`, so that is the
    /// variable that is read when nothing else is said.
    pub fn default_key_env(&self) -> Option<&'static str> {
        match self {
            Self::OpenAi => None,
            Self::ElevenLabs => Some("ELEVENLABS_API_KEY"),
        }
    }
}

/// Something that says words aloud.
#[derive(Debug, Clone)]
pub enum Speaker {
    /// A program that reads the words on its standard input and speaks them.
    Command {
        /// The program.
        program: String,
        /// Its arguments.
        args: Vec<String>,
    },
    /// A program that reads the words on its standard input and writes a
    /// wav on its standard output, which is then played.
    WavCommand {
        /// The program.
        program: String,
        /// Its arguments.
        args: Vec<String>,
        /// The player that makes the sound.
        player: Player,
    },
    /// An endpoint that answers with audio.
    Http {
        /// How the endpoint wants to be spoken to.
        api: ApiStyle,
        /// Where to send the words. For an ElevenLabs voice this may be
        /// the base of its API, or the full address of the voice's own
        /// endpoint; the rest is worked out from it.
        url: String,
        /// The key, if the endpoint wants one.
        key: Option<String>,
        /// The model the endpoint should use.
        model: String,
        /// The voice the endpoint should use.
        voice: Option<String>,
        /// The player that makes the sound.
        player: Player,
    },
    /// An in-process neural speech model powered by Phonon (`ptts`).
    Phonon {
        /// The Phonon speaker.
        speaker: std::sync::Arc<super::phonon::PhononSpeaker>,
    },
}

impl Speaker {
    /// Build an in-process Phonon speaker.
    ///
    /// A player is only needed to make a sound. Synthesis into a file or an
    /// HTTP response works without one, which a headless gateway needs.
    pub fn phonon(
        model: Option<String>,
        lang: Option<&str>,
        voice: Option<String>,
        player: Option<Player>,
    ) -> Self {
        Speaker::Phonon {
            speaker: std::sync::Arc::new(super::phonon::PhononSpeaker::new(
                model, lang, voice, player,
            )),
        }
    }

    /// Whether the speaker is really there.
    ///
    /// A program that is named in the configuration may not be installed,
    /// and saying so is better than failing at the moment something is
    /// meant to be said.
    pub fn exists(&self) -> bool {
        match self {
            Speaker::Command { program, .. } => detect::which(program).is_some(),
            Speaker::WavCommand {
                program, player, ..
            } => detect::which(program).is_some() && player.exists(),
            Speaker::Http { player, .. } => player.exists(),
            Speaker::Phonon { speaker } => speaker.player().is_some_and(|player| player.exists()),
        }
    }

    /// Whether this speaker can turn words into sound at all.
    ///
    /// Unlike [`Self::exists`], this only asks whether the words can be made
    /// into audio: Phonon needs no player, and a program that writes a wav
    /// needs only the program itself. The gateway serves that audio; a tool
    /// that speaks aloud still needs [`Self::exists`].
    pub fn can_synthesize(&self) -> bool {
        match self {
            Speaker::Phonon { .. } => true,
            Speaker::WavCommand { program, .. } => detect::which(program).is_some(),
            _ => false,
        }
    }

    /// Turn the words into WAV bytes without playing them.
    ///
    /// This is what the gateway hands to a browser, so the page can hear the
    /// same voice as the terminal even when the voice is a program such as
    /// piper. A speaker that only speaks for itself has nothing to hand
    /// over.
    pub async fn wav(&self, text: &str) -> Result<Vec<u8>> {
        let text = text.trim();
        if text.is_empty() {
            bail!("there are no words to say");
        }
        match self {
            Speaker::WavCommand { program, args, .. } => make_wav(program, args, text).await,
            _ => bail!("the configured speaker cannot write a wav"),
        }
    }

    /// Say the words.
    pub async fn speak(&self, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        match self {
            Speaker::Command { program, args } => run_command(program, args, text).await,
            Speaker::WavCommand {
                program,
                args,
                player,
            } => {
                let audio = make_wav(program, args, text).await?;
                play(player, &audio).await
            }
            Speaker::Http {
                api,
                url,
                key,
                model,
                voice,
                player,
            } => {
                let audio = match api {
                    ApiStyle::OpenAi => {
                        post_text(url, key.as_deref(), model, voice.as_deref(), text).await?
                    }
                    ApiStyle::ElevenLabs => {
                        post_elevenlabs(url, key.as_deref(), model, voice.as_deref(), text).await?
                    }
                };
                play(player, &audio).await
            }
            Speaker::Phonon { speaker } => speaker.speak(text).await,
        }
    }

    /// What this speaker is, for the doctor.
    pub fn describe(&self) -> String {
        match self {
            Speaker::Command { program, .. } => program.clone(),
            Speaker::WavCommand { program, .. } => program.clone(),
            Speaker::Http { url, .. } => url.clone(),
            Speaker::Phonon { .. } => "phonon".to_string(),
        }
    }

    /// The name of the voice this speaker speaks with, if it has one.
    pub fn model_name(&self) -> Option<String> {
        match self {
            Speaker::Command { .. } | Speaker::Http { .. } => None,
            Speaker::WavCommand { args, .. } => detect::model_name(args),
            Speaker::Phonon { speaker } => Some(speaker.model_name()),
        }
    }
}

/// Run a program that speaks the words itself.
async fn run_command(program: &str, args: &[String], text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not start {program}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.shutdown().await;
    }

    let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .context("speaking took too long")?
        .with_context(|| format!("{program} could not be run"))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{program} could not speak{}",
            if error.is_empty() {
                String::new()
            } else {
                format!(": {error}")
            }
        );
    }
    Ok(())
}

/// Run a program that turns the words into a wav.
async fn make_wav(program: &str, args: &[String], text: &str) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not start {program}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.shutdown().await;
    }

    let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .context("making the sound took too long")?
        .with_context(|| format!("{program} could not be run"))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{program} could not make the sound{}",
            if error.is_empty() {
                String::new()
            } else {
                format!(": {error}")
            }
        );
    }
    Ok(output.stdout)
}

/// Send the words to the OpenAI shape of a speech endpoint.
async fn post_text(
    url: &str,
    key: Option<&str>,
    model: &str,
    voice: Option<&str>,
    text: &str,
) -> Result<Vec<u8>> {
    let mut body = serde_json::json!({
        "model": model,
        "input": text,
    });
    if let Some(voice) = voice {
        body["voice"] = serde_json::Value::String(voice.to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .context("could not make the http client")?;

    let mut request = client.post(url).json(&body);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("{url} answered {status}: {}", body.trim());
    }

    let audio = response.bytes().await.context("could not read the sound")?;
    if audio.is_empty() {
        bail!("{url} answered with no sound");
    }
    Ok(audio.to_vec())
}

/// Send the words to ElevenLabs and get the sound back.
///
/// The voice is part of the address, and the key rides in a header of its
/// own; the model is named `model_id` in the body, where the OpenAI shape
/// would call it `model`. This is the request J.A.R.V.I.S's TTS module
/// makes with ElevenLabs' own library, written out in the ordinary way so
/// no library of theirs is needed.
async fn post_elevenlabs(
    url: &str,
    key: Option<&str>,
    model: &str,
    voice: Option<&str>,
    text: &str,
) -> Result<Vec<u8>> {
    // The voice has to be named somewhere. When the address does not carry
    // it, the voice J.A.R.V.I.S speaks with is used, so that naming an
    // endpoint and a key is enough to be heard.
    let voice = voice
        .filter(|voice| !voice.trim().is_empty())
        .unwrap_or(JARVIS_VOICE);
    let endpoint = elevenlabs_endpoint(url, voice);

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .context("could not make the http client")?;

    let mut request = client
        .post(&endpoint)
        .json(&serde_json::json!({ "text": text, "model_id": model }));
    if let Some(key) = key.filter(|key| !key.trim().is_empty()) {
        // The key is not a bearer here: it heads the request by its name.
        request = request.header("xi-api-key", key);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("could not reach {endpoint}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("{endpoint} answered {status}: {}", body.trim());
    }

    let audio = response.bytes().await.context("could not read the sound")?;
    if audio.is_empty() {
        bail!("{endpoint} answered with no sound");
    }
    Ok(audio.to_vec())
}

/// The voice of J.A.R.V.I.S, which its module speaks with when no other is
/// named. Kept as the fallback, so that a voice of its own kind is opened
/// with no more than an address and a key.
const JARVIS_VOICE: &str = "xFjhlCVIoEAjDeZpAmFe";

/// The address of the endpoint that speaks in one voice.
///
/// The base is what the configuration names: the API alone
/// (`https://api.elevenlabs.io`), its first version
/// (`https://api.elevenlabs.io/v1`), or the voice's own endpoint already
/// whole. The first two gain the path that says voice; the last is left
/// as it is.
fn elevenlabs_endpoint(base: &str, voice: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.contains("/text-to-speech/") {
        return base.to_string();
    }
    let base = if base.ends_with("/v1") {
        base.to_string()
    } else {
        format!("{base}/v1")
    };
    format!("{base}/text-to-speech/{voice}")
}

/// Turn writing into something that can be said.
///
/// The marks that make writing readable — the fence around a piece of code,
/// the stars around a word, the brackets around a link — are of no use to
/// a voice, and a voice that reads them out says things that nobody wrote.
/// They are taken away, and what is left is prose.
pub fn plain(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut in_code = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            // The fence of a piece of code: what is inside it is for the
            // eye, not for the ear.
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        // A table is read as nothing at all: its rules and its bars are
        // there to be looked at.
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            continue;
        }
        // A rule of its own, and a row of dashes.
        if trimmed
            .chars()
            .all(|c| c == '-' || c == '=' || c == '*' || c == '_')
        {
            continue;
        }
        let line = strip_marks(&strip_bullets(trimmed));
        if !line.is_empty() {
            parts.push(line);
        }
    }

    parts.join(" ")
}

/// Take away the marks that stand in front of a line.
fn strip_bullets(line: &str) -> String {
    let mut line = line.to_string();
    // The marks of a heading.
    while line.starts_with('#') {
        line.remove(0);
    }
    // The marks of a quotation.
    while line.starts_with('>') {
        line.remove(0);
    }
    let line = line.trim_start().to_string();
    // The mark of a list.
    for mark in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(mark) {
            return rest.to_string();
        }
    }
    // The number of a numbered list.
    if let Some((number, rest)) = line.split_once(". ")
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
    {
        return rest.to_string();
    }
    line
}

/// Take away the marks that are within a line.
///
/// A link is read by its words and not by its address, and the stars and
/// the backticks are not read at all.
fn strip_marks(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                // The words of a link are kept, and its address is not.
                let mut words = String::new();
                let mut depth = 1;
                for c in chars.by_ref() {
                    match c {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    words.push(c);
                }
                if chars.peek() == Some(&'(') {
                    for c in chars.by_ref() {
                        if c == ')' {
                            break;
                        }
                    }
                }
                out.push_str(words.trim());
            }
            // The marks of bold, of italics and of code.
            '*' | '`' | '~' => continue,
            _ => out.push(c),
        }
    }
    // The spaces that are left over are made one.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Play some sound.
///
/// The sound is written to a file of its own, and the player is given the
/// name of it: the players that read a wav want a file, and the ones that
/// read raw samples do not read a wav at all. A player that is given no
/// name gets the sound on its standard input instead.
pub async fn play(player: &Player, audio: &[u8]) -> Result<()> {
    let (program, args) = player
        .command
        .split_first()
        .context("the playing command is empty")?;

    // The sound goes to a file of its own, and is taken away afterwards.
    let path = std::env::temp_dir().join(format!("lia-voice-{}.wav", uuid::Uuid::new_v4()));
    std::fs::write(&path, audio).with_context(|| format!("could not write {}", path.display()))?;

    let mut arguments = Vec::with_capacity(args.len());
    let mut named = false;
    for arg in args {
        if arg == detect::SOUND_PLACEHOLDER {
            named = true;
            arguments.push(path.to_string_lossy().to_string());
        } else {
            arguments.push(arg.clone());
        }
    }

    let mut command = Command::new(program);
    command
        .args(&arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !named {
        command.stdin(Stdio::piped());
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {program}"))?;

    if !named && let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(audio).await;
        let _ = stdin.shutdown().await;
    }

    let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .context("the sound took too long to play")?
        .with_context(|| format!("{program} could not be run"))?;

    // The sound has been played, and the file is of no use now.
    let _ = std::fs::remove_file(&path);

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{program} could not play the sound{}",
            if error.is_empty() {
                String::new()
            } else {
                format!(": {error}")
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevenlabs_is_named_by_its_address() {
        assert_eq!(
            ApiStyle::named(None, "https://api.elevenlabs.io/v1"),
            ApiStyle::ElevenLabs
        );
        assert_eq!(
            ApiStyle::named(None, "http://127.0.0.1:8080/v1/audio/speech"),
            ApiStyle::OpenAi
        );
    }

    #[test]
    fn elevenlabs_is_named_in_the_configuration() {
        assert_eq!(
            ApiStyle::named(Some("elevenlabs"), "http://127.0.0.1:1"),
            ApiStyle::ElevenLabs
        );
        assert_eq!(
            ApiStyle::named(Some("Eleven Labs"), "http://127.0.0.1:1"),
            ApiStyle::ElevenLabs
        );
        // But a name said outright wins over an address.
        assert_eq!(
            ApiStyle::named(Some("openai"), "https://api.elevenlabs.io"),
            ApiStyle::OpenAi
        );
    }

    #[test]
    fn an_address_gains_the_path_that_names_the_voice() {
        // The API alone: the path of the first version and of the voice.
        assert_eq!(
            elevenlabs_endpoint("https://api.elevenlabs.io", "abc"),
            "https://api.elevenlabs.io/v1/text-to-speech/abc"
        );
        // The version already named: the voice is still to be.
        assert_eq!(
            elevenlabs_endpoint("https://api.elevenlabs.io/v1", "abc"),
            "https://api.elevenlabs.io/v1/text-to-speech/abc"
        );
        // The endpoint whole: nothing is added to it.
        assert_eq!(
            elevenlabs_endpoint("https://api.elevenlabs.io/v1/text-to-speech/abc", "xyz"),
            "https://api.elevenlabs.io/v1/text-to-speech/abc"
        );
    }

    #[test]
    fn the_default_model_is_the_one_each_endpoint_understands() {
        assert_eq!(ApiStyle::OpenAi.default_model(), "tts-1");
        assert_eq!(ApiStyle::ElevenLabs.default_model(), "eleven_turbo_v2");
    }
}
