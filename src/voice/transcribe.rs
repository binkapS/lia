//! Words from sound.
//!
//! Two ways of doing it: a program that reads a wav on its standard input
//! and writes the words on its standard output — whisper.cpp's
//! `whisper-cli`, which is what is used on this machine — and an HTTP
//! endpoint that takes the wav as a form and answers with the words, in the
//! shape whisper-server and OpenAI both use.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use super::detect;
use super::record::read_wav;

/// How long a transcription may take.
const TIMEOUT: Duration = Duration::from_secs(300);

/// The placeholder in the arguments that stands for the wav file.
///
/// A program that is given the sound by name has the path put in its place.
/// A program without the placeholder reads the sound on its standard input
/// instead.
const FILE_PLACEHOLDER: &str = "{file}";

/// Something that turns sound into words.
#[derive(Debug, Clone)]
pub enum Transcriber {
    /// A program that reads a wav on its standard input.
    Command {
        /// The program.
        program: String,
        /// Its arguments.
        args: Vec<String>,
    },
    /// An endpoint that takes the wav as a form.
    Http {
        /// Where to send it.
        url: String,
        /// The key, if the endpoint wants one.
        key: Option<String>,
        /// The model the endpoint should use.
        model: String,
        /// The language to transcribe in.
        language: Option<String>,
    },
}

impl Transcriber {
    /// Whether the transcriber is really there.
    ///
    /// A program that is named in the configuration may not be installed,
    /// and saying so is better than failing at the moment something is
    /// meant to be heard.
    pub fn exists(&self) -> bool {
        match self {
            Transcriber::Command { program, .. } => detect::which(program).is_some(),
            Transcriber::Http { .. } => true,
        }
    }

    /// Turn a wav file into words.
    pub async fn transcribe(&self, wav: &Path) -> Result<String> {
        match self {
            Transcriber::Command { program, args } => run_command(program, args, wav).await,
            Transcriber::Http {
                url,
                key,
                model,
                language,
            } => {
                let audio = read_wav(wav)?;
                post_form(url, key.as_deref(), model, language.as_deref(), audio).await
            }
        }
    }

    /// What this transcriber is, for the doctor.
    pub fn describe(&self) -> String {
        match self {
            Transcriber::Command { program, .. } => program.clone(),
            Transcriber::Http { url, .. } => url.clone(),
        }
    }
}

/// Run a program that turns the wav into words.
async fn run_command(program: &str, args: &[String], wav: &Path) -> Result<String> {
    // The program is either given the file by name, or it reads the sound
    // on its standard input.
    let mut arguments: Vec<String> = Vec::with_capacity(args.len());
    let mut named = false;
    for arg in args {
        if arg == FILE_PLACEHOLDER {
            named = true;
            arguments.push(wav.to_string_lossy().to_string());
        } else {
            arguments.push(arg.clone());
        }
    }

    let mut command = Command::new(program);
    command
        .args(&arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !named {
        command.stdin(Stdio::piped());
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {program}"))?;

    // The wav goes in on the standard input, so the program is given one
    // file rather than a path it would have to be able to read.
    if !named {
        let audio = read_wav(wav)?;
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(&audio).await;
            let _ = stdin.shutdown().await;
        }
    }

    let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .context("the transcription took too long")?
        .with_context(|| format!("{program} could not be run"))?;

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() && text.is_empty() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{program} failed{}{}",
            if error.is_empty() { "" } else { ": " },
            error
        );
    }
    Ok(text)
}

/// Send the wav to an endpoint as a form.
async fn post_form(
    url: &str,
    key: Option<&str>,
    model: &str,
    language: Option<&str>,
    audio: Vec<u8>,
) -> Result<String> {
    let part = reqwest::multipart::Part::bytes(audio)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .context("could not make the form")?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.to_string());
    if let Some(language) = language {
        form = form.text("language", language.to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .context("could not make the http client")?;

    let mut request = client.post(url).multipart(form);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?;

    let status = response.status();
    let body = response.text().await.context("could not read the answer")?;
    if !status.is_success() {
        bail!("{url} answered {status}: {}", body.trim());
    }

    // The usual answer is `{"text": "..."}`, but a plain body of words is
    // taken as the words themselves.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body)
        && let Some(text) = value.get("text").and_then(|t| t.as_str())
    {
        return Ok(text.trim().to_string());
    }
    Ok(body.trim().to_string())
}
