//! Capture.
//!
//! The microphone is read by a program that writes raw 16 kHz mono 16-bit
//! PCM to its standard output. The header of a wav file is written here, so
//! whatever records the sound, the transcriber is always handed a standard
//! file.
//!
//! While the sound is coming in, the level is measured, so the interface can
//! show that it is hearing something.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::watch;

/// The rate the sound is recorded at.
///
/// 16 kHz is what the speech engines want, and it keeps the recording small.
pub const SAMPLE_RATE: u32 = 16_000;

/// How many channels are recorded. One: the microphone is not a stereo one.
pub const CHANNELS: u16 = 1;

/// The number of bytes in one sample of one channel.
const BYTES_PER_SAMPLE: usize = 2;

/// The quietest sound that still counts as somebody speaking.
///
/// Below this the microphone is only picking up the room.
const SPEECH_RMS: f32 = 0.008;

/// A recording that has been made.
#[derive(Debug, Clone)]
pub struct Recording {
    /// Where the sound was written.
    pub path: PathBuf,
    /// How long it was, in seconds.
    pub seconds: f64,
    /// The loudest moment of it, 0..=1.
    pub peak: f32,
    /// Whether anybody was speaking in it.
    pub heard_speech: bool,
}

/// How the recording should go.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    /// How long the recording may last.
    pub max_seconds: u64,
    /// How long a stretch of silence ends the recording, if the recording
    /// is meant to end by itself.
    ///
    /// This is what makes the hands-free mode work: the recording ends
    /// when the person stops talking, rather than at a fixed time.
    pub silence: Option<Duration>,
}

impl Default for RecordOptions {
    fn default() -> Self {
        RecordOptions {
            max_seconds: 60,
            silence: None,
        }
    }
}

/// The microphone.
#[derive(Debug, Clone)]
pub struct Recorder {
    /// The command that records, writing raw PCM to its standard output.
    command: Vec<String>,
    /// The rate the sound comes in at.
    sample_rate: u32,
    /// How many channels come in.
    channels: u16,
}

impl Recorder {
    /// A recorder that runs the given command.
    pub fn new(command: Vec<String>) -> Self {
        Recorder {
            command,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
        }
    }

    /// The command this recorder runs.
    pub fn command(&self) -> &[String] {
        &self.command
    }

    /// Record until the recording is stopped, or the time runs out.
    ///
    /// `stop` is watched for a change: when it is set, the recorder is
    /// stopped by its own pid. `on_level` is called with the level of the
    /// sound, 0..=1, as it arrives.
    pub async fn record(
        &self,
        options: RecordOptions,
        mut stop: watch::Receiver<bool>,
        mut on_level: impl FnMut(f32) + Send,
    ) -> Result<Recording> {
        let (program, args) = self
            .command
            .split_first()
            .context("the recording command is empty")?;

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("could not start {program}"))?;

        let mut stdout = child
            .stdout
            .take()
            .context("the recorder has no standard output")?;

        // What the recorder says is kept, so that a recorder that gives up
        // can say why.
        let mut stderr = child
            .stderr
            .take()
            .context("the recorder has no error output")?;
        let complaints = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        });

        let mut raw: Vec<u8> = Vec::new();
        let mut peak: f32 = 0.0;
        let mut heard_speech = false;
        let mut silent_for = Duration::ZERO;
        let start = Instant::now();
        let limit = Duration::from_secs(options.max_seconds.clamp(1, 600));

        loop {
            let mut chunk = vec![0u8; 4096];
            let read = tokio::select! {
                result = stdout.read(&mut chunk) => result.context("could not read the microphone")?,
                // The recording was stopped. The child is killed by its pid.
                _ = stop.changed() => {
                    stop_child(&mut child).await;
                    break;
                }
                _ = tokio::time::sleep(limit.saturating_sub(start.elapsed())) => {
                    stop_child(&mut child).await;
                    break;
                }
            };

            if read == 0 {
                // The recorder ended on its own.
                break;
            }
            chunk.truncate(read);

            let level = level_of(&chunk);
            peak = peak.max(level);
            if rms(&chunk) >= SPEECH_RMS {
                heard_speech = true;
                silent_for = Duration::ZERO;
            } else {
                silent_for += Duration::from_secs_f64(
                    chunk.len() as f64 / (self.sample_rate as f64 * self.channels as f64),
                );
            }
            on_level(level);

            raw.extend_from_slice(&chunk);

            // The silence is only an ending once somebody has spoken, or
            // the recording would stop before it began.
            if let Some(limit_silence) = options.silence
                && heard_speech
                && silent_for >= limit_silence
            {
                stop_child(&mut child).await;
                break;
            }
        }

        // What the recorder had already written is read out, so that the
        // last moment of the sound is not lost when the recorder is stopped
        // before it has said all it was going to say. The wait is bounded,
        // because a recorder that left a child behind would hold the pipe
        // open.
        let mut rest = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_millis(200), stdout.read_to_end(&mut rest)).await;
        if !rest.is_empty() {
            let level = level_of(&rest);
            peak = peak.max(level);
            if rms(&rest) >= SPEECH_RMS {
                heard_speech = true;
            }
            on_level(level);
            raw.extend_from_slice(&rest);
        }

        // How the recorder ended, and what it said about it. Both are only
        // asked about when the recording is empty, so a recorder that worked
        // does not have to say anything. The waiting is bounded, because a
        // recorder that left a child behind would hold the pipe open.
        let status = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        let complaints = tokio::time::timeout(Duration::from_secs(2), complaints)
            .await
            .ok()
            .and_then(|joined| joined.ok())
            .unwrap_or_default();
        if raw.is_empty() {
            let reason = complaints.lines().last().unwrap_or("it said nothing");
            let how = match &status {
                Ok(Ok(status)) => format!("it ended with {status}"),
                Ok(Err(e)) => format!("it could not be waited for: {e}"),
                Err(_) => "it was still running".to_string(),
            };
            bail!("{program} gave no sound: {reason} ({how})");
        }

        let seconds = raw.len() as f64
            / (self.sample_rate as f64 * self.channels as f64 * BYTES_PER_SAMPLE as f64);
        let path = write_wav(&raw, self.sample_rate, self.channels)?;

        Ok(Recording {
            path,
            seconds,
            peak,
            heard_speech,
        })
    }
}

/// Stop the recorder, by its pid.
async fn stop_child(child: &mut Child) {
    // `kill` sends the signal to that one process, and the child is ours.
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// The loudness of a chunk of samples, 0..=1.
///
/// The scale is in decibels rather than in raw amplitude, because that is
/// how a level meter should behave: the quiet parts are small and the loud
/// parts fill the bar without clipping.
pub fn level_of(bytes: &[u8]) -> f32 {
    let rms = rms(bytes);
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// The root mean square of a chunk of samples, 0..=1.
pub fn rms(bytes: &[u8]) -> f32 {
    let mut sum = 0f64;
    let mut count = 0u64;
    for sample in bytes.as_chunks::<BYTES_PER_SAMPLE>().0 {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as f64 / 32768.0;
        sum += value * value;
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    ((sum / count as f64).sqrt()) as f32
}

/// Write raw samples as a wav file.
pub fn write_wav(raw: &[u8], sample_rate: u32, channels: u16) -> Result<PathBuf> {
    let mut header = Vec::with_capacity(44);
    let data_len = raw.len() as u32;
    let byte_rate = sample_rate * channels as u32 * BYTES_PER_SAMPLE as u32;
    let block_align = channels * BYTES_PER_SAMPLE as u16;

    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());

    let path = std::env::temp_dir().join(format!("lia-voice-{}.wav", uuid::Uuid::new_v4()));
    let mut file = std::fs::File::create(&path)
        .with_context(|| format!("could not write {}", path.display()))?;
    std::io::Write::write_all(&mut file, &header).context("could not write the wav header")?;
    std::io::Write::write_all(&mut file, raw).context("could not write the sound")?;
    Ok(path)
}

/// Read a wav file.
///
/// The whole file is read, header and all: a transcriber is given a wav,
/// not a heap of samples.
pub fn read_wav(path: &std::path::Path) -> Result<Vec<u8>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" {
        bail!("{} is not a wav file", path.display());
    }
    Ok(bytes)
}

/// The samples of a wav file, without the header.
pub fn samples(path: &std::path::Path) -> Result<Vec<u8>> {
    let bytes = read_wav(path)?;
    // Find the data chunk, so a file with anything before it still works.
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]])
            as usize;
        let body = at + 8;
        if id == b"data" {
            let end = (body + size).min(bytes.len());
            return Ok(bytes[body..end].to_vec());
        }
        at = body + size + (size % 2);
    }
    Ok(bytes[44..].to_vec())
}
