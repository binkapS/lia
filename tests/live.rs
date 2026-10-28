//! Live mode: the microphone stays open, and talking over the answer stops
//! it.
//!
//! The sound hardware is stood in for, as in the voice tests: a script
//! records, another transcribes, another speaks. What is checked here is the
//! turn-taking around them, and the frame that goes with a message.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lia::config::{Config, LiveConfig, LiveShare, VoiceConfig};
use lia::tui::app::Line;
use lia::tui::{App, handle_key};
use lia::voice::VoiceEvent;

/// A directory of its own for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lia-live-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("the directory is made");
    dir
}

/// Write a program that can be run, and return its path.
fn program(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, script).expect("the program is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path)
            .expect("the program is there")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("the permissions are set");
    }
    path
}

/// A stand-in for the microphone: it writes a second of sound and stops.
fn fake_recorder(dir: &Path) -> PathBuf {
    program(
        dir,
        "fake-recorder",
        r#"#!/bin/sh
i=0
while [ $i -lt 16000 ]; do
  printf '\001\000'
  i=$((i + 1))
done
"#,
    )
}

/// A stand-in for the transcriber: it answers with words.
fn fake_transcriber(dir: &Path) -> PathBuf {
    program(
        dir,
        "fake-transcriber",
        r#"#!/bin/sh
cat > /dev/null
echo "what the person said"
"#,
    )
}

/// A stand-in for the speaker: it writes down what it was told to say.
fn fake_speaker(dir: &Path) -> PathBuf {
    program(
        dir,
        "fake-speaker",
        &format!(
            r#"#!/bin/sh
cat >> {}
"#,
            dir.join("said.txt").display()
        ),
    )
}

/// An interface that can hear and speak.
fn app(dir: &Path) -> App {
    let config = Config {
        voice: VoiceConfig {
            enabled: true,
            record_command: Some(vec![fake_recorder(dir).display().to_string()]),
            transcribe_command: Some(vec![fake_transcriber(dir).display().to_string()]),
            speak_command: Some(vec![fake_speaker(dir).display().to_string()]),
            max_seconds: 5,
            ..VoiceConfig::default()
        },
        ..Config::default()
    };
    let mut app = App::new(config);
    app.start_voice();
    app
}

/// Press a key.
async fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    handle_key(app, KeyEvent::new(code, modifiers)).await;
}

/// Type a word.
async fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c), KeyModifiers::NONE).await;
    }
}

/// Run a slash command.
async fn run(app: &mut App, command: &str) {
    type_text(app, command).await;
    press(app, KeyCode::Enter, KeyModifiers::NONE).await;
}

/// Whether a user line with these words is in the transcript.
fn sent(app: &App, text: &str) -> bool {
    app.transcript
        .iter()
        .any(|line| matches!(line, Line::User(said) if said == text))
}

#[tokio::test]
async fn live_mode_sends_what_is_said_without_the_wake_word() {
    let dir = scratch("no-wake-word");
    let mut app = app(&dir);
    app.live = true;

    app.apply_voice_event(VoiceEvent::Transcript {
        text: "what is the weather".into(),
    })
    .await;

    assert!(
        sent(&app, "what is the weather"),
        "the words were not sent: {:?}",
        app.transcript
    );
}

#[tokio::test]
async fn live_mode_passes_over_her_own_voice() {
    let dir = scratch("echo");
    let mut app = app(&dir);
    app.live = true;

    // What she is saying now is kept, so the open microphone hearing it
    // back can be recognised.
    app.speak("hello there my friend");

    app.apply_voice_event(VoiceEvent::Transcript {
        text: "hello there my friend".into(),
    })
    .await;

    assert!(
        !app.transcript
            .iter()
            .any(|line| matches!(line, Line::User(_))),
        "her own voice was answered: {:?}",
        app.transcript
    );
}

#[tokio::test]
async fn talking_over_the_answer_stops_it_and_keeps_the_words() {
    let dir = scratch("interrupt");
    let mut app = app(&dir);
    app.live = true;

    app.speak("hello there my friend");
    assert!(app.is_speaking(), "the answer was not being said");

    app.apply_voice_event(VoiceEvent::Transcript {
        text: "what is the weather".into(),
    })
    .await;

    assert!(
        !app.is_speaking(),
        "the answer went on after the interruption"
    );
    assert!(
        sent(&app, "what is the weather"),
        "the interruption was not sent: {:?}",
        app.transcript
    );
    assert!(
        app.transcript
            .iter()
            .any(|line| format!("{line:?}").contains("interrupted")),
        "the person was not told that the answer stopped: {:?}",
        app.transcript
    );
}

#[tokio::test]
async fn escape_stops_the_answer_without_closing_the_microphone() {
    let dir = scratch("escape");
    let mut app = app(&dir);
    app.live = true;
    app.start_listening(false);

    app.speak("hello there my friend");
    app.escape();

    assert!(!app.is_speaking(), "the answer went on after escape");
    assert!(
        app.is_listening(),
        "escape closed the microphone in live mode"
    );
}

#[tokio::test]
async fn the_live_command_turns_the_mode_on_and_off() {
    let dir = scratch("command");
    let mut app = app(&dir);

    run(&mut app, "/live on").await;
    assert!(app.live, "live mode is not on");
    assert!(app.is_listening(), "the microphone was not opened");
    assert!(!app.handsfree, "hands-free was left on as well");

    run(&mut app, "/live off").await;
    assert!(!app.live, "live mode is still on");
    assert!(!app.is_listening(), "the microphone was left open");
}

#[tokio::test]
async fn the_live_command_chooses_what_is_shared() {
    let dir = scratch("share");
    let mut app = app(&dir);

    run(&mut app, "/live camera").await;
    assert!(app.live, "live mode is not on");
    assert_eq!(app.live_share, LiveShare::Camera);

    run(&mut app, "/live screen").await;
    assert_eq!(app.live_share, LiveShare::Screen);

    run(&mut app, "/live none").await;
    assert_eq!(app.live_share, LiveShare::None);
    assert!(app.live, "voice-only turned live mode off");
}

#[tokio::test]
async fn the_live_section_is_read_from_the_configuration() {
    let dir = scratch("config");
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        r#"
[live]
share = "screen"
camera_command = ["cam", "{file}"]
screen_command = ["shot", "{file}"]
echo_similarity = 0.8
capture_timeout_secs = 3
"#,
    )
    .expect("the configuration is written");

    let config = Config::load_file(&path).expect("the configuration is read");
    assert_eq!(config.live.share, LiveShare::Screen);
    assert_eq!(
        config.live.camera_command.as_deref(),
        Some(&["cam".to_string(), "{file}".to_string()][..])
    );
    assert_eq!(
        config.live.screen_command.as_deref(),
        Some(&["shot".to_string(), "{file}".to_string()][..])
    );
    assert_eq!(config.live.echo_similarity, 0.8);
    assert_eq!(config.live.capture_timeout_secs, 3);
}

#[tokio::test]
async fn a_frame_is_taken_by_the_configured_command() {
    let dir = scratch("frame");
    let camera = program(
        &dir,
        "fake-camera",
        r#"#!/bin/sh
printf 'a picture' > "$1"
"#,
    );

    let config = LiveConfig {
        camera_command: Some(vec![camera.display().to_string()]),
        ..LiveConfig::default()
    };

    let image = lia::live::capture_frame(&config, LiveShare::Camera)
        .await
        .expect("the picture is taken");
    assert_eq!(image.media_type, "image/png");
    assert!(!image.data.is_empty(), "the picture is empty");
}

#[tokio::test]
async fn a_frame_command_can_be_given_the_path_in_the_placeholder() {
    let dir = scratch("placeholder");
    let camera = program(
        &dir,
        "fake-camera",
        r#"#!/bin/sh
printf 'a picture' > "$2"
"#,
    );

    let config = LiveConfig {
        camera_command: Some(vec![
            camera.display().to_string(),
            "--out".to_string(),
            "{file}".to_string(),
        ]),
        ..LiveConfig::default()
    };

    let image = lia::live::capture_frame(&config, LiveShare::Camera)
        .await
        .expect("the picture is taken");
    assert!(!image.data.is_empty(), "the picture is empty");
}
