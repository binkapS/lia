//! Tests of the voice.
//!
//! The sound hardware is stood in for: a shell script records, another
//! transcribes, another speaks, and the real code cannot tell the
//! difference. The HTTP backends are asked of a small server that answers
//! the way a transcription endpoint does.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::HeaderMap;
use axum::routing::post;
use lia::config::VoiceConfig;
use lia::voice::record::{level_of, read_wav, rms, samples, write_wav};
use lia::voice::{RecordOptions, Voice};
use serde_json::Value;

/// A directory of its own for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lia-voice-{name}-{}", uuid::Uuid::new_v4()));
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

/// A stand-in for a recorder: it writes a second of sound and stops.
fn fake_recorder(dir: &Path) -> PathBuf {
    // 16 kHz, mono, 16 bit: a second is 32000 bytes. The wave is a tone,
    // so the level is not zero.
    program(
        dir,
        "fake-recorder",
        r#"#!/bin/sh
# A second of a quiet tone, written as raw 16-bit samples.
i=0
while [ $i -lt 16000 ]; do
  printf '\001\000'
  i=$((i + 1))
done
"#,
    )
}

/// A stand-in for a transcriber: it checks it was given a wav and answers
/// with words.
fn fake_transcriber(dir: &Path) -> PathBuf {
    program(
        dir,
        "fake-transcriber",
        r#"#!/bin/sh
# The wav comes on the standard input. It is only a wav if it starts
# with RIFF.
if head -c 4 | grep -q RIFF; then
  echo "the microphone works"
else
  echo "that was not a wav"
fi
"#,
    )
}

/// A stand-in for a speaker: it writes down what it was told to say.
fn fake_speaker(dir: &Path, record: &Path) -> PathBuf {
    program(
        dir,
        "fake-speaker",
        &format!(
            r#"#!/bin/sh
cat > {}
"#,
            record.display()
        ),
    )
}

/// A stand-in for a transcriber that is given the file by name.
fn fake_transcriber_by_name(dir: &Path) -> PathBuf {
    program(
        dir,
        "fake-transcriber-by-name",
        r#"#!/bin/sh
# The wav is named by one of the arguments: the one that is a file.
path=""
for arg in "$@"; do
  if [ -f "$arg" ]; then path="$arg"; fi
done
if [ -n "$path" ] && head -c 4 "$path" | grep -q RIFF; then
  echo "the named file works"
else
  echo "that was not a wav"
fi
"#,
    )
}

/// A voice that uses the stand-ins.
fn a_voice(config: VoiceConfig) -> Voice {
    Voice::new(config)
}

/// The configuration that uses the stand-ins.
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

#[tokio::test]
async fn a_recording_becomes_words() {
    let dir = scratch("words");
    let voice = a_voice(stand_in_config(&dir));

    let text = voice
        .listen(RecordOptions {
            max_seconds: 5,
            silence: None,
        })
        .await
        .expect("the sound was heard");

    assert_eq!(text, "the microphone works");
}

#[tokio::test]
async fn the_words_are_said_aloud() {
    let dir = scratch("speak");
    let config = stand_in_config(&dir);
    let voice = a_voice(config);

    voice.say("hello there").await.expect("the words were said");

    let said = std::fs::read_to_string(dir.join("said.txt")).expect("the words were written");
    assert_eq!(said.trim(), "hello there");
}

#[tokio::test]
async fn the_microphone_is_only_opened_once() {
    let dir = scratch("once");
    let config = stand_in_config(&dir);
    let voice = Arc::new(a_voice(config));

    // Two listenings at the same time: the second waits for the first.
    let first = {
        let voice = voice.clone();
        tokio::spawn(async move {
            voice
                .listen(RecordOptions {
                    max_seconds: 5,
                    silence: None,
                })
                .await
        })
    };
    let second = {
        let voice = voice.clone();
        tokio::spawn(async move {
            voice
                .listen(RecordOptions {
                    max_seconds: 5,
                    silence: None,
                })
                .await
        })
    };

    let one = first.await.expect("the task finished").expect("it heard");
    let two = second.await.expect("the task finished").expect("it heard");
    assert_eq!(one, "the microphone works");
    assert_eq!(two, "the microphone works");
}

#[test]
fn a_wav_file_is_a_wav_file() {
    let raw: Vec<u8> = (0..1000u16).map(|i| (i % 256) as u8).collect();
    let path = write_wav(&raw, 16_000, 1).expect("the wav is written");

    let bytes = std::fs::read(&path).expect("the wav is there");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    // The sample rate is at byte 24, little-endian.
    assert_eq!(&bytes[24..28], &16_000u32.to_le_bytes());
    // The channels are at byte 22.
    assert_eq!(&bytes[22..24], &1u16.to_le_bytes());
    // The data chunk holds the samples.
    assert_eq!(&bytes[36..40], b"data");

    // The whole file is given to a transcriber, and the samples come back
    // unchanged.
    let whole = read_wav(&path).expect("the wav is read");
    assert_eq!(whole.len(), 44 + raw.len());
    assert_eq!(samples(&path).expect("the samples are read"), raw);
}

#[test]
fn the_level_follows_the_sound() {
    // Silence.
    let quiet = vec![0u8; 4000];
    assert_eq!(rms(&quiet), 0.0);
    assert_eq!(level_of(&quiet), 0.0);

    // A loud tone, at about half of the full scale.
    let mut loud = Vec::new();
    for _ in 0..2000 {
        loud.extend_from_slice(&16_000i16.to_le_bytes());
    }
    let level = level_of(&loud);
    assert!(level > 0.5, "a loud sound should fill the meter: {level}");
    assert!(level <= 1.0);
}

#[test]
fn a_quiet_sound_is_still_below_a_loud_one() {
    let mut faint = Vec::new();
    for _ in 0..2000 {
        faint.extend_from_slice(&100i16.to_le_bytes());
    }
    let mut loud = Vec::new();
    for _ in 0..2000 {
        loud.extend_from_slice(&16_000i16.to_le_bytes());
    }
    assert!(level_of(&faint) < level_of(&loud));
}

/// The server, and the address it answers on.
async fn serve(state: Arc<State2>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let app = Router::new()
        .route("/v1/audio/transcriptions", post(transcribe))
        .route("/v1/audio/speech", post(speech))
        .route("/v1/text-to-speech/{voice}", post(speech_of_a_voice))
        .with_state(state);

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{address}")
}

/// What the server has seen.
#[derive(Clone, Default)]
struct State2 {
    /// The bodies of the requests that arrived.
    seen: Arc<Mutex<Vec<Value>>>,
    /// The headers of the last request.
    headers: Arc<Mutex<Option<HeaderMap>>>,
    /// The voice named in the last address.
    voice: Arc<Mutex<Option<String>>>,
    /// The sound the speech endpoint answers with.
    audio: Arc<Mutex<Vec<u8>>>,
}

/// Answer a transcription request.
async fn transcribe(State(state): State<Arc<State2>>, headers: HeaderMap, body: Bytes) -> String {
    *state.headers.lock().unwrap() = Some(headers);
    // The body is a form; the interesting part is that one arrived.
    let text = String::from_utf8_lossy(&body).to_string();
    state.seen.lock().unwrap().push(Value::String(text));
    r#"{"text":"the words from the endpoint"}"#.to_string()
}

/// Answer a speech request.
async fn speech(State(state): State<Arc<State2>>, body: Bytes) -> Vec<u8> {
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.seen.lock().unwrap().push(parsed);
    state.audio.lock().unwrap().clone()
}

/// Answer a speech request that names a voice in its address, the way
/// ElevenLabs does: the voice and the headers are kept alongside the body.
async fn speech_of_a_voice(
    State(state): State<Arc<State2>>,
    UrlPath(voice): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Vec<u8> {
    *state.headers.lock().unwrap() = Some(headers);
    *state.voice.lock().unwrap() = Some(voice);
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.seen.lock().unwrap().push(parsed);
    state.audio.lock().unwrap().clone()
}

#[tokio::test]
async fn the_voice_of_elevenlabs_is_asked_for_the_sound() {
    // The key is in the environment, the way J.A.R.V.I.S keeps it.
    unsafe {
        std::env::set_var("LIA_TEST_ELEVEN_KEY", "its-key");
    }
    let state = Arc::new(State2::default());
    *state.audio.lock().unwrap() = vec![1, 2, 3, 4];
    let address = serve(state.clone()).await;

    let dir = scratch("elevenlabs-speak");
    let record = dir.join("played.bin");
    let config = VoiceConfig {
        enabled: true,
        // The voice is named in a configuration, and the base of the API
        // alone: the address of the voice's own endpoint is worked out
        // from it.
        speak_url: Some(address),
        speak_provider: Some("elevenlabs".into()),
        // The variable is named, the way the configuration means it.
        speak_api_key_env: Some("LIA_TEST_ELEVEN_KEY".into()),
        play_command: Some(vec![
            program(
                &dir,
                "fake-player",
                &format!("#!/bin/sh\ncat > {}\n", record.display()),
            )
            .display()
            .to_string(),
        ]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);

    voice.say("hello there").await.expect("the words were said");

    // The voice named in the configuration is part of the address.
    let asked = state
        .voice
        .lock()
        .unwrap()
        .clone()
        .expect("a voice was named");
    assert_eq!(asked, "xFjhlCVIoEAjDeZpAmFe");

    let seen = state.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "the endpoint was asked once");
    assert_eq!(seen[0]["text"], "hello there");
    assert_eq!(seen[0]["model_id"], "eleven_turbo_v2");
    assert!(seen[0].get("input").is_none(), "the words are not `input`");

    // The key rides in a header of its own rather than as a bearer.
    let headers = state.headers.lock().unwrap().clone().unwrap();
    assert_eq!(
        headers.get("xi-api-key").and_then(|v| v.to_str().ok()),
        Some("its-key")
    );

    let played = std::fs::read(&record).expect("the sound was played");
    assert_eq!(played, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn the_endpoint_is_asked_for_the_words() {
    let state = Arc::new(State2::default());
    let address = serve(state.clone()).await;

    let dir = scratch("http-transcribe");
    let config = VoiceConfig {
        enabled: true,
        transcribe_url: Some(format!("{address}/v1/audio/transcriptions")),
        record_command: Some(vec![fake_recorder(&dir).display().to_string()]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);

    let text = voice
        .listen(RecordOptions {
            max_seconds: 5,
            silence: None,
        })
        .await
        .expect("the sound was heard");

    assert_eq!(text, "the words from the endpoint");

    let seen = state.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "the endpoint was asked once");
    assert!(
        seen[0].as_str().unwrap_or("").contains("RIFF"),
        "the wav was sent as a form"
    );
}

#[tokio::test]
async fn the_endpoint_is_asked_for_the_sound() {
    let state = Arc::new(State2::default());
    *state.audio.lock().unwrap() = vec![1, 2, 3, 4];
    let address = serve(state.clone()).await;

    let dir = scratch("http-speak");
    let record = dir.join("played.bin");
    let config = VoiceConfig {
        enabled: true,
        speak_url: Some(format!("{address}/v1/audio/speech")),
        play_command: Some(vec![
            program(
                &dir,
                "fake-player",
                &format!("#!/bin/sh\ncat > {}\n", record.display()),
            )
            .display()
            .to_string(),
        ]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);

    voice.say("hello there").await.expect("the words were said");

    let seen = state.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "the endpoint was asked once");
    assert_eq!(seen[0]["input"], "hello there");

    let played = std::fs::read(&record).expect("the sound was played");
    assert_eq!(played, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn a_file_named_in_the_arguments_is_read() {
    let dir = scratch("named-file");
    let config = VoiceConfig {
        enabled: true,
        record_command: Some(vec![fake_recorder(&dir).display().to_string()]),
        // The placeholders are filled in with the name of the wav.
        transcribe_command: Some(vec![
            fake_transcriber_by_name(&dir).display().to_string(),
            "-f".to_string(),
            "{file}".to_string(),
        ]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);

    let text = voice
        .listen(RecordOptions {
            max_seconds: 5,
            silence: None,
        })
        .await
        .expect("the sound was heard");

    assert_eq!(text, "the named file works");
}

#[tokio::test]
async fn a_voice_without_a_recorder_cannot_listen() {
    let config = VoiceConfig {
        enabled: true,
        record_command: Some(vec!["/nonexistent/recorder".into()]),
        ..VoiceConfig::default()
    };
    // The configuration names a program that is not there, so nothing is
    // found.
    let voice = a_voice(config);
    assert!(!voice.can_listen());
    assert!(
        voice.listen(RecordOptions::default()).await.is_err(),
        "there is nothing to listen with"
    );
}

/// A stand-in for a speech engine that makes a wav: it writes the words
/// into the wav, so that the test can see what was said.
fn fake_wav_speaker(dir: &Path) -> PathBuf {
    program(
        dir,
        "fake-wav-speaker",
        r#"#!/bin/sh
words="$(cat)"
printf 'RIFF'
printf '%s' "$words"
"#,
    )
}

/// A stand-in for the player: it writes down what it was asked to play.
fn fake_player(dir: &Path, record: &Path) -> PathBuf {
    program(
        dir,
        "fake-player",
        &format!(
            r#"#!/bin/sh
cat > {}
"#,
            record.display()
        ),
    )
}

#[tokio::test]
async fn a_wav_is_made_and_played() {
    let dir = scratch("wav");
    let played = dir.join("played.wav");
    let config = VoiceConfig {
        enabled: true,
        speak_wav_command: Some(vec![fake_wav_speaker(&dir).display().to_string()]),
        play_command: Some(vec![fake_player(&dir, &played).display().to_string()]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);

    voice.say("hello there").await.expect("the words were said");

    let sound = std::fs::read_to_string(&played).expect("the sound was played");
    assert!(
        sound.contains("hello there"),
        "the sound did not hold the words: {sound:?}"
    );
}

#[tokio::test]
async fn a_wav_program_can_be_synthesized_for_a_client() {
    // The gateway serves this path: piper writes the wav, so the browser
    // hears the same voice as the terminal without a sound card.
    let dir = scratch("wav-client");
    let config = VoiceConfig {
        enabled: true,
        speak_wav_command: Some(vec![fake_wav_speaker(&dir).display().to_string()]),
        play_command: Some(vec![
            fake_player(&dir, &dir.join("played.wav"))
                .display()
                .to_string(),
        ]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);

    assert!(voice.can_synthesize());
    assert!(voice.can_speak());

    let audio = voice
        .synthesize_wav_async("hello there", None)
        .await
        .expect("the wav was made");
    let audio = String::from_utf8_lossy(&audio);
    assert!(audio.starts_with("RIFF"), "that is not a wav: {audio:?}");
    assert!(
        audio.contains("hello there"),
        "the words were lost: {audio:?}"
    );
}

#[test]
fn the_writing_is_turned_into_something_that_can_be_said() {
    // The marks of writing are not read out loud.
    let spoken = lia::voice::speak::plain(
        "Here is the **answer**:\n\n```rust\nlet x = 1;\n```\n\n- first\n- second\n\nSee [the docs](https://example.com).",
    );
    assert!(!spoken.contains("**"), "the stars were read: {spoken:?}");
    assert!(!spoken.contains("let x"), "the code was read: {spoken:?}");
    assert!(!spoken.contains("```"), "the fence was read: {spoken:?}");
    assert!(spoken.contains("first"), "the list was lost: {spoken:?}");
    assert!(
        spoken.contains("the docs") && !spoken.contains("example.com"),
        "the link was read: {spoken:?}"
    );
}

#[test]
fn only_the_words_that_name_lia_are_for_lia() {
    // The name comes first: what follows it is the message.
    assert_eq!(
        lia::voice::addressed_to("Lia, what is the weather?", "lia"),
        Some("what is the weather?".to_string())
    );
    assert_eq!(
        lia::voice::addressed_to("hey lia what time is it", "lia"),
        Some("what time is it".to_string())
    );
    // Words that do not name her are not for her.
    assert_eq!(
        lia::voice::addressed_to("what is the weather?", "lia"),
        None
    );
    assert_eq!(lia::voice::addressed_to("social studies", "lia"), None);
    assert_eq!(lia::voice::addressed_to("", "lia"), None);
    // Only her name was said, so there is nothing to answer.
    assert_eq!(lia::voice::addressed_to("lia", "lia"), Some(String::new()));
}

/// Phonon is part of lia: it can turn words into sound even on a machine
/// with no player, which is what the gateway needs. Without a player it
/// cannot be heard aloud.
#[test]
fn phonon_can_synthesize_without_a_player() {
    let config = VoiceConfig {
        enabled: true,
        speak_provider: Some("phonon".into()),
        speak_model: Some("hf://gradium/phonon-31m-en".into()),
        // Named, so that a player on the machine running the test does not
        // turn this into a test with one.
        play_command: Some(vec!["/nonexistent/lia-no-player".into()]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);

    assert!(voice.can_synthesize());
    assert!(!voice.can_speak(), "there is no player to hear it with");
}

/// With a player, Phonon is the voice that is found, and it is reported by
/// the checkpoint it will speak with.
#[test]
fn phonon_is_found_when_it_is_configured() {
    let dir = scratch("phonon");
    let played = dir.join("played.wav");
    let config = VoiceConfig {
        enabled: true,
        speak_provider: Some("phonon".into()),
        speak_model: Some("hf://gradium/phonon-31m-en".into()),
        play_command: Some(vec![fake_player(&dir, &played).display().to_string()]),
        ..VoiceConfig::default()
    };
    let voice = a_voice(config);
    let capabilities = voice.capabilities();

    assert!(voice.can_speak());
    assert_eq!(capabilities.speaker.as_deref(), Some("phonon"));
    assert_eq!(
        capabilities.voice_model.as_deref(),
        Some("gradium/phonon-31m-en")
    );
}
