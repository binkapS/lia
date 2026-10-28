//! The voice in the interface.
//!
//! The sound hardware is stood in for: a shell script records, another
//! transcribes, another speaks. The interface is driven by keys, and the
//! words are read from the voice as they arrive.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lia::config::VoiceConfig;
use lia::tui::app::VoiceState;
use lia::tui::{App, handle_key};
use lia::voice::VoiceEvent;

/// A directory of its own for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lia-tui-voice-{name}-{}", uuid::Uuid::new_v4()));
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

/// A stand-in for a microphone that keeps going until it is stopped.
fn endless_recorder(dir: &Path) -> PathBuf {
    program(
        dir,
        "endless-recorder",
        r#"#!/bin/sh
while true; do
  # a second of sound, in one piece
  head -c 32000 /dev/zero | tr '\000' '\100'
  sleep 0.1
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
fn fake_speaker(dir: &Path, record: &Path) -> PathBuf {
    program(
        dir,
        "fake-speaker",
        &format!(
            r#"#!/bin/sh
cat >> {}
"#,
            record.display()
        ),
    )
}

/// The voice the interface is given.
fn stand_in_config(dir: &Path) -> VoiceConfig {
    VoiceConfig {
        enabled: true,
        record_command: Some(vec![fake_recorder(dir).display().to_string()]),
        transcribe_command: Some(vec![fake_transcriber(dir).display().to_string()]),
        speak_command: Some(vec![
            fake_speaker(dir, &dir.join("said.txt"))
                .display()
                .to_string(),
        ]),
        max_seconds: 5,
        ..VoiceConfig::default()
    }
}

/// An interface that can hear and speak.
fn app(dir: &Path) -> App {
    let config = lia::Config {
        voice: stand_in_config(dir),
        ..lia::Config::default()
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

/// Wait for the voice to say something, and apply it.
async fn next_event(app: &mut App) {
    let worker = app.voice_worker.as_mut().expect("the voice is working");
    let event = worker.recv().await.expect("the voice reported");
    app.apply_voice_event(event).await;
}

/// Wait for the recording to end, and apply everything said on the way.
///
/// The voice reports the level of the sound as it comes in, and those
/// reports are passed over: what matters here is the end of the recording.
async fn wait_for_the_words(app: &mut App) {
    loop {
        let worker = app.voice_worker.as_mut().expect("the voice is working");
        let event = worker.recv().await.expect("the voice reported");
        let is_level = matches!(event, VoiceEvent::Listening { .. });
        app.apply_voice_event(event).await;
        if !is_level {
            return;
        }
    }
}

/// Wait until the sound has been coming in for a while.
///
/// A recording that is stopped has to be long enough to be worth turning
/// into words, so the wait is for the clock rather than for the first
/// sound.
async fn wait_for_the_sound(app: &mut App) {
    loop {
        let worker = app.voice_worker.as_mut().expect("the voice is working");
        let event = worker.recv().await.expect("the voice reported");
        let elapsed = match &event {
            VoiceEvent::Listening { elapsed, .. } => Some(*elapsed),
            _ => None,
        };
        app.apply_voice_event(event).await;
        if elapsed.is_some_and(|elapsed| elapsed >= 0.3) {
            return;
        }
    }
}

/// An interface whose microphone never stops on its own.
fn endless_app(dir: &Path) -> App {
    let config = lia::Config {
        voice: VoiceConfig {
            enabled: true,
            record_command: Some(vec![endless_recorder(dir).display().to_string()]),
            transcribe_command: Some(vec![fake_transcriber(dir).display().to_string()]),
            speak_command: Some(vec![
                fake_speaker(dir, &dir.join("said.txt"))
                    .display()
                    .to_string(),
            ]),
            max_seconds: 5,
            ..VoiceConfig::default()
        },
        ..lia::Config::default()
    };
    let mut app = App::new(config);
    app.start_voice();
    app
}

#[tokio::test]
async fn the_microphone_opens_with_the_f2_key() {
    let dir = scratch("f2");
    let mut app = app(&dir);

    assert!(!app.is_listening());
    press(&mut app, KeyCode::F(2), KeyModifiers::NONE).await;
    assert!(app.is_listening(), "F2 did not open the microphone");
}

#[tokio::test]
async fn the_words_are_sent_as_a_message() {
    let dir = scratch("words");
    let mut app = app(&dir);

    press(&mut app, KeyCode::F(2), KeyModifiers::NONE).await;
    wait_for_the_words(&mut app).await;

    // The recording is over, and the words are the person's message.
    assert!(!app.is_listening(), "the microphone was left open");
    assert!(
        app.transcript.iter().any(
            |line| matches!(line, lia::tui::app::Line::User(text) if text == "what the person said")
        ),
        "the words were not sent: {:?}",
        app.transcript
    );
    // The box is empty again, because the words were sent.
    assert_eq!(app.input.text(), "");
}

#[tokio::test]
async fn a_stopped_recording_still_becomes_words() {
    let dir = scratch("stopped");
    let mut app = endless_app(&dir);

    press(&mut app, KeyCode::F(2), KeyModifiers::NONE).await;
    wait_for_the_sound(&mut app).await;

    // The second F2 stops the recording, and what was said is kept.
    press(&mut app, KeyCode::F(2), KeyModifiers::NONE).await;
    wait_for_the_words(&mut app).await;

    assert!(
        app.transcript.iter().any(
            |line| matches!(line, lia::tui::app::Line::User(text) if text == "what the person said")
        ),
        "the words were lost when the recording was stopped: {:?}",
        app.transcript
    );
}

#[tokio::test]
async fn a_cancelled_recording_is_thrown_away() {
    let dir = scratch("cancelled");
    let mut app = endless_app(&dir);

    press(&mut app, KeyCode::F(2), KeyModifiers::NONE).await;
    wait_for_the_sound(&mut app).await;

    // The escape key throws the sound away rather than keeping it.
    app.escape();
    wait_for_the_words(&mut app).await;

    assert!(
        !app.transcript.iter().any(
            |line| matches!(line, lia::tui::app::Line::User(text) if text == "what the person said")
        ),
        "the words were kept although the recording was cancelled: {:?}",
        app.transcript
    );
    assert!(
        app.transcript
            .iter()
            .any(|line| format!("{line:?}").contains("thrown away")),
        "the person was not told that the recording was thrown away: {:?}",
        app.transcript
    );
}

#[tokio::test]
async fn the_level_is_shown_while_the_sound_comes_in() {
    let dir = scratch("level");
    let mut app = app(&dir);
    app.start_listening(false);

    app.apply_voice_event(VoiceEvent::Listening {
        level: 0.5,
        elapsed: 1.5,
    })
    .await;

    assert!(matches!(
        app.voice_state,
        VoiceState::Listening { level, elapsed } if level == 0.5 && elapsed == 1.5
    ));
    assert_eq!(app.voice_levels, vec![0.5]);
}

#[tokio::test]
async fn a_report_that_comes_too_late_is_passed_over() {
    let dir = scratch("late");
    let mut app = app(&dir);

    // The microphone was never opened, so the report is of no use: it
    // would put the interface back into a recording that is not going on.
    app.apply_voice_event(VoiceEvent::Listening {
        level: 0.5,
        elapsed: 1.5,
    })
    .await;

    assert!(!app.is_listening(), "a late report opened the microphone");
    assert!(app.voice_levels.is_empty(), "a late report was shown");
}

#[tokio::test]
async fn the_meter_keeps_only_the_last_moments() {
    let dir = scratch("meter");
    let mut app = app(&dir);

    for _ in 0..40 {
        app.apply_voice_event(VoiceEvent::Listening {
            level: 0.5,
            elapsed: 1.0,
        })
        .await;
    }

    assert!(app.voice_levels.len() <= 24, "the meter grew without end");
}

#[tokio::test]
async fn a_failure_is_shown() {
    let dir = scratch("failure");
    let mut app = app(&dir);

    app.apply_voice_event(VoiceEvent::Failed("the microphone is busy".into()))
        .await;

    let said = app
        .transcript
        .iter()
        .any(|line| format!("{line:?}").contains("the microphone is busy"));
    assert!(said, "the failure was not shown");
}

#[tokio::test]
async fn the_dictation_goes_into_the_box_and_not_into_a_message() {
    let dir = scratch("dictate");
    let mut app = app(&dir);

    app.start_listening(true);
    wait_for_the_words(&mut app).await;

    // The words are there to be edited, and nothing was sent.
    assert_eq!(app.input.text(), "what the person said");
    assert!(app.history.is_empty(), "the words were sent on their own");
}

#[tokio::test]
async fn a_new_interface_stays_quiet() {
    let dir = scratch("voice-default");
    let app = app(&dir);

    assert!(!app.speak_replies, "the terminal is silent by default");
    assert!(!app.handsfree, "the microphone listens by default");
}

#[tokio::test]
async fn the_voice_command_switches_the_spoken_replies_on_and_off() {
    let dir = scratch("voice-command");
    let mut app = app(&dir);

    type_text(&mut app, "/voice on").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.speak_replies, "the replies are still silent");

    type_text(&mut app, "/voice off").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(!app.speak_replies, "the replies are still spoken");
}

#[tokio::test]
async fn the_hands_free_command_turns_the_loop_on() {
    let dir = scratch("handsfree");
    let mut app = app(&dir);

    type_text(&mut app, "/handsfree on").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(app.handsfree, "the loop is not running");

    type_text(&mut app, "/handsfree off").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert!(!app.handsfree, "the loop is still running");
}

#[tokio::test]
async fn what_is_said_is_said_out_loud() {
    let dir = scratch("spoken");
    let mut app = app(&dir);

    app.speak("hello there");
    next_event(&mut app).await;

    let said = std::fs::read_to_string(dir.join("said.txt")).expect("the words were said");
    assert_eq!(said.trim(), "hello there");
}

#[tokio::test]
async fn the_answer_is_said_while_it_is_still_arriving() {
    use lia::agent::AgentEvent;

    let dir = scratch("streamed-speech");
    let mut app = app(&dir);
    app.speak_replies = true;

    app.apply_event(AgentEvent::Started {
        model: "test-model".into(),
    })
    .await;
    app.apply_event(AgentEvent::TextDelta(
        "The first sentence is ready. The second is still being written".into(),
    ))
    .await;

    // The first sentence has already been spoken, without waiting for the
    // turn to finish.
    next_event(&mut app).await;
    let said = std::fs::read_to_string(dir.join("said.txt")).expect("the first words were said");
    assert!(said.contains("The first sentence is ready."));
    assert!(
        !said.contains("The second"),
        "the unfinished sentence was spoken too early: {said}"
    );

    app.apply_event(AgentEvent::Finished {
        messages: Vec::new(),
        usage: None,
        iterations: 1,
    })
    .await;
    next_event(&mut app).await;
    let said = std::fs::read_to_string(dir.join("said.txt")).expect("the tail was said");
    assert!(
        said.contains("The second is still being written"),
        "the rest of the answer was not said: {said}"
    );
}

#[tokio::test]
async fn the_loop_listens_again_after_the_answer_has_been_said() {
    let dir = scratch("loop");
    let mut app = app(&dir);

    app.handsfree = true;
    app.apply_voice_event(VoiceEvent::Spoken).await;

    // The microphone is open again, so lia does not hear itself.
    assert!(app.is_listening(), "the microphone was not opened again");
}

#[tokio::test]
async fn in_hands_free_mode_the_words_that_name_lia_are_answered() {
    let dir = scratch("wake");
    let mut app = app(&dir);
    app.handsfree = true;

    // The name comes first, and what follows it is the message.
    app.apply_voice_event(VoiceEvent::Transcript {
        text: "Lia, what is the weather".into(),
    })
    .await;

    assert!(
        app.transcript.iter().any(
            |line| matches!(line, lia::tui::app::Line::User(text) if text == "what is the weather")
        ),
        "the words were not sent: {:?}",
        app.transcript
    );
}

#[tokio::test]
async fn in_hands_free_mode_the_words_that_do_not_name_lia_are_passed_over() {
    let dir = scratch("wake-over");
    let mut app = app(&dir);
    app.handsfree = true;

    // The talk of the room is not for lia.
    app.apply_voice_event(VoiceEvent::Transcript {
        text: "what is the weather".into(),
    })
    .await;

    assert!(
        !app.transcript
            .iter()
            .any(|line| matches!(line, lia::tui::app::Line::User(_))),
        "the words were sent although they did not name lia: {:?}",
        app.transcript
    );
    // The loop goes on: the microphone is open again.
    assert!(app.is_listening(), "the microphone was not opened again");
}

#[tokio::test]
async fn only_the_name_of_lia_is_not_enough_to_answer() {
    let dir = scratch("wake-only");
    let mut app = app(&dir);
    app.handsfree = true;

    app.apply_voice_event(VoiceEvent::Transcript { text: "lia".into() })
        .await;

    assert!(
        !app.transcript
            .iter()
            .any(|line| matches!(line, lia::tui::app::Line::User(_))),
        "the name alone was sent: {:?}",
        app.transcript
    );
    assert!(app.is_listening(), "the microphone was not opened again");
}

#[tokio::test]
async fn outside_hands_free_mode_everything_that_is_said_is_sent() {
    let dir = scratch("no-wake");
    let mut app = app(&dir);

    app.apply_voice_event(VoiceEvent::Transcript {
        text: "what is the weather".into(),
    })
    .await;

    assert!(
        app.transcript.iter().any(
            |line| matches!(line, lia::tui::app::Line::User(text) if text == "what is the weather")
        ),
        "the words were not sent: {:?}",
        app.transcript
    );
}
