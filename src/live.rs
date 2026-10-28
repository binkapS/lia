//! Live mode: a conversation that does not wait its turn.
//!
//! The Gemini app calls this Live: the microphone stays open, the answer is
//! spoken while the person is still there, and talking over the assistant
//! stops it. The shipped Android app does not implement that itself — it
//! hands `liveconv`, `livevideo` and `livescreen` to the Google app — so
//! what is built here is the same behaviour with the pieces lia already
//! has: a recorder, a transcriber, a speaker, and the agent.
//!
//! Two small parts are shared by the terminal and the command line:
//!
//! - [`looks_like_echo`] keeps lia from answering her own voice, which the
//!   open microphone hears while she is speaking.
//! - [`capture_frame`] takes the one picture the model is shown when the
//!   camera or the screen is being shared.
//!
//! The turn-taking itself belongs to the interface, because that is where
//! the transcript, the agent and the keys already are.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::{LiveConfig, LiveShare};
use crate::image::Image;

/// The placeholder in a capture command that stands for the picture's path.
const FILE_PLACEHOLDER: &str = "{file}";

/// Whether what was heard is lia's own voice coming back through the room.
///
/// The microphone is open while the speaker is working, so some of what it
/// hears is the answer itself. A transcript is taken as an echo when most
/// of its words are words that were just said. A person who talks over her
/// brings words of their own, and those words are not in the answer.
pub fn looks_like_echo(heard: &str, spoken: &[String], similarity: f64) -> bool {
    let heard = words(heard);
    if heard.is_empty() {
        return false;
    }

    // The guard is off when the threshold is zero.
    if similarity <= 0.0 {
        return false;
    }

    let said: Vec<String> = spoken.iter().flat_map(|line| words(line)).collect();
    if said.is_empty() {
        return false;
    }

    let matching = heard
        .iter()
        .filter(|word| said.iter().any(|said| said == *word))
        .count();
    let share = matching as f64 / heard.len() as f64;
    // More than the threshold, not merely as much: a person who talks over
    // the answer usually repeats a few of her words, and those words alone
    // must not make their question look like an echo.
    share > similarity.clamp(0.0, 1.0)
}

/// The words of a piece of text, in lower case.
///
/// A word has to have two letters or a digit in it, so the punctuation of
/// speech and the "a" and "I" that every sentence has do not make two
/// different sentences look alike.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| {
            !word.is_empty()
                && (word.chars().count() > 1 || word.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|word| word.to_lowercase())
        .collect()
}

/// Take one picture from the camera or the screen.
///
/// The command is given the path in `{file}`; a command without the
/// placeholder has the path put at the end, which is the shape `grim` and
/// `import` use. The picture is read and the temporary file removed.
pub async fn capture_frame(config: &LiveConfig, share: LiveShare) -> Result<Image> {
    let command = match share {
        LiveShare::None => bail!("neither the camera nor the screen is being shared"),
        LiveShare::Camera => config
            .camera_command
            .as_ref()
            .context("no camera command is configured (live.camera_command)")?,
        LiveShare::Screen => config
            .screen_command
            .as_ref()
            .context("no screen command is configured (live.screen_command)")?,
    };

    let path = temp_picture_path();
    let (program, args) = command
        .split_first()
        .context("the capture command is empty")?;

    let mut arguments: Vec<String> = Vec::with_capacity(args.len() + 1);
    let mut named = false;
    for arg in args {
        if arg == FILE_PLACEHOLDER {
            named = true;
            arguments.push(path.to_string_lossy().to_string());
        } else {
            arguments.push(arg.clone());
        }
    }
    if !named {
        arguments.push(path.to_string_lossy().to_string());
    }

    let child = tokio::process::Command::new(program)
        .args(&arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not start {program}"))?;

    let timeout = Duration::from_secs(config.capture_timeout_secs.clamp(1, 120));
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => result.with_context(|| format!("{program} could not be run"))?,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            bail!("the picture took too long");
        }
    };

    if !output.status.success() {
        let _ = std::fs::remove_file(&path);
        let complaint = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "{program} could not take the picture{}",
            if complaint.is_empty() {
                String::new()
            } else {
                format!(": {complaint}")
            }
        );
    }

    let image = Image::from_file(&path);
    let _ = std::fs::remove_file(&path);
    image.map_err(|e| anyhow::anyhow!(e))
}

/// A path for a picture that is about to be taken.
fn temp_picture_path() -> PathBuf {
    std::env::temp_dir().join(format!("lia-live-{}.png", uuid::Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_answer_is_heard_as_an_echo() {
        let spoken = vec!["The weather in Paris is sunny today.".to_string()];
        assert!(looks_like_echo(
            "the weather in paris is sunny",
            &spoken,
            0.6
        ));
    }

    #[test]
    fn a_person_who_talks_over_the_answer_is_not_an_echo() {
        let spoken = vec!["The weather in Paris is sunny today.".to_string()];
        assert!(!looks_like_echo("stop, what about tomorrow", &spoken, 0.6));
    }

    #[test]
    fn a_mixed_voice_is_still_an_interruption_when_it_brings_new_words() {
        let spoken = vec!["The weather in Paris is sunny today.".to_string()];
        assert!(!looks_like_echo(
            "the weather in Paris is sunny, and what about Rome",
            &spoken,
            0.6
        ));
    }

    #[test]
    fn the_guard_can_be_switched_off() {
        let spoken = vec!["hello there".to_string()];
        assert!(!looks_like_echo("hello there", &spoken, 0.0));
    }

    #[test]
    fn the_picture_path_is_a_png() {
        assert_eq!(
            temp_picture_path().extension().and_then(|e| e.to_str()),
            Some("png")
        );
    }
}
