//! Which programs are on this machine.
//!
//! The voice backends are ordinary programs: something records the
//! microphone, something turns the sound into words, and something says them
//! aloud. None of them are bundled, so lia looks for them on the `PATH` and
//! uses the first one it finds. The configuration can name a different one
//! when the first is not the right one.

use std::path::{Path, PathBuf};

/// Find a program on the `PATH`.
///
/// A name with a slash in it is looked for where it says, so a program can
/// be given by its full path.
pub fn which(program: &str) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }

    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(program);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Whether a path is a file that can be run.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// The recorders, in the order they are tried.
///
/// Every one of them writes raw 16 kHz mono 16-bit PCM to its standard
/// output, which is what the rest of the voice code expects.
pub const RECORDERS: &[(&str, &[&str])] = &[
    (
        "pw-record",
        &["--format", "s16", "--rate", "16000", "--channels", "1", "-"],
    ),
    (
        "arecord",
        &[
            "-D", "default", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "raw", "-",
        ],
    ),
    (
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "alsa",
            "-i",
            "default",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-f",
            "s16le",
            "-",
        ],
    ),
    (
        "sox",
        &[
            "-t", "raw", "-r", "16000", "-e", "signed", "-b", "16", "-c", "1", "-",
        ],
    ),
];

/// Find a recorder.
///
/// The program and its arguments are returned apart: the program is run,
/// and the arguments are given to it. The program is not repeated among the
/// arguments, because a recorder that takes a target would take the name of
/// the program as its target, and record nothing at all.
pub fn record_command() -> Option<(String, Vec<String>)> {
    record_command_with(which)
}

/// Find a recorder, with the search for the program given.
fn record_command_with(find: impl Fn(&str) -> Option<PathBuf>) -> Option<(String, Vec<String>)> {
    for (program, args) in RECORDERS {
        if find(program).is_some() {
            return Some((
                program.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
            ));
        }
    }
    None
}

/// The transcribers, in the order they are tried.
///
/// Each of them writes the words on its standard output. A program that
/// takes the wav by name has `{file}` in its arguments; a program that reads
/// it on its standard input has no placeholder.
pub const TRANSCRIBERS: &[(&str, &[&str])] = &[
    // whisper.cpp: the model is added when one is found.
    //
    // The file is named rather than piped in, because whisper-cli only
    // writes the words to its standard output when it is given a file to
    // read: with the sound on its standard input it treats the name "-" as
    // the output too, and says nothing at all.
    ("whisper-cli", &["-f", "{file}", "-nt"]),
    // The python whisper: the model is added when one is found.
    (
        "whisper",
        &["-f", "-", "--output_format", "txt", "--output", "-"],
    ),
];

/// Find a transcriber that also has a model to work with.
///
/// A transcriber without a model is no use, so one is only reported when
/// both are there. As with the recorder, the program is returned apart from
/// its arguments.
pub fn transcribe_command(model: Option<&Path>) -> Option<(String, Vec<String>)> {
    transcribe_command_with(model, which)
}

/// Find a transcriber, with the search for the program given.
fn transcribe_command_with(
    model: Option<&Path>,
    find: impl Fn(&str) -> Option<PathBuf>,
) -> Option<(String, Vec<String>)> {
    for (program, args) in TRANSCRIBERS {
        if find(program).is_none() {
            continue;
        }
        let model = match model {
            Some(model) => model.to_path_buf(),
            None => match default_model(program) {
                Some(model) => model,
                // A transcriber without a model is no use.
                None => continue,
            },
        };
        let mut command = Vec::new();
        if *program == "whisper-cli" {
            command.push("-m".to_string());
            command.push(model.to_string_lossy().to_string());
        } else {
            command.push("--model".to_string());
            command.push(model.to_string_lossy().to_string());
        }
        command.extend(args.iter().map(|a| a.to_string()));
        return Some((program.to_string(), command));
    }
    None
}

/// Find a model for a transcriber, in the places lia looks.
fn default_model(program: &str) -> Option<PathBuf> {
    let names: &[&str] = match program {
        "whisper-cli" => &[
            "ggml-base.en.bin",
            "ggml-base.bin",
            "ggml-small.en.bin",
            "ggml-small.bin",
        ],
        _ => &["base.en", "base", "small.en", "small"],
    };

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".cache").join("lia"));
        roots.push(home.join(".cache").join("whisper"));
        roots.push(home.join(".local").join("share").join("lia").join("models"));
        roots.push(home.join(".local").join("share").join("whisper.cpp"));
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        roots.push(PathBuf::from(dir).join("lia").join("models"));
    }
    roots.push(
        PathBuf::from("/usr/share")
            .join("whisper.cpp")
            .join("models"),
    );

    for root in roots {
        for name in names {
            let candidate = root.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The speakers, in the order they are tried.
///
/// Each of them reads the words on its standard input and says them aloud.
/// The `-e` of `spd-say` is its pipe mode: the text is read from the
/// standard input, echoed to the standard output, and spoken.
pub const SPEAKERS: &[(&str, &[&str])] = &[
    ("spd-say", &["-e", "-w"]),
    ("espeak-ng", &["-w", "-"]),
    ("piper", &[]),
    ("say", &["-o", "-"]),
];

/// Find a speaker.
///
/// As with the recorder, the program is returned apart from its arguments:
/// a speaker that takes the words to say as an argument would say the name
/// of the program instead of them.
pub fn speak_command() -> Option<(String, Vec<String>)> {
    speak_command_with(which)
}

/// Find a speaker, with the search for the program given.
fn speak_command_with(find: impl Fn(&str) -> Option<PathBuf>) -> Option<(String, Vec<String>)> {
    for (program, args) in SPEAKERS {
        if find(program).is_none() {
            continue;
        }
        // `piper` needs a model of its own, and `say` writes a wav rather
        // than speaking, so neither is chosen here.
        if matches!(*program, "piper" | "say") {
            continue;
        }
        return Some((
            program.to_string(),
            args.iter().map(|a| a.to_string()).collect(),
        ));
    }
    None
}

/// The speakers that write a wav, in the order they are tried.
///
/// Each of them reads the words on its standard input and writes the sound
/// on its standard output, which is then played. A speaker of this kind can
/// be a neural voice, which sounds like a person rather than a machine.
pub const WAV_SPEAKERS: &[(&str, &[&str])] = &[
    // piper: the model is added when one is found, because a voice without
    // a model is no voice at all. The `-f -` is what makes it write the
    // wav to its standard output rather than to a file of its own.
    ("piper", &["--model", "{model}", "-f", "-"]),
    // espeak-ng writes a wav rather than speaking for itself.
    ("espeak-ng", &["-w", "-"]),
    // say, on a mac.
    ("say", &["-o", "-"]),
];

/// The placeholder in the arguments that stands for the voice model.
const MODEL_PLACEHOLDER: &str = "{model}";

/// The name of the voice model among the arguments, if there is one.
///
/// The name is what the model file is called, without the folders and the
/// suffix: `en_US-hfc_female-medium`, for a voice of a woman.
pub fn model_name(args: &[String]) -> Option<String> {
    args.iter().find_map(|arg| {
        let path = Path::new(arg);
        if path.extension().is_none_or(|extension| extension != "onnx") {
            return None;
        }
        path.file_stem()
            .map(|name| name.to_string_lossy().to_string())
    })
}

/// Find a speaker that writes a wav.
///
/// The voice is named by `model`, when the configuration names one. When it
/// does not, the voices are looked for in the places lia keeps its models,
/// and the one that sounds most like a person is used.
pub fn speak_wav_command(
    model: Option<&Path>,
    options: &[String],
) -> Option<(String, Vec<String>)> {
    speak_wav_command_with(model, options, which)
}

/// Find a speaker that writes a wav, with the search for the program given.
fn speak_wav_command_with(
    model: Option<&Path>,
    options: &[String],
    find: impl Fn(&str) -> Option<PathBuf>,
) -> Option<(String, Vec<String>)> {
    for (program, args) in WAV_SPEAKERS {
        if find(program).is_none() {
            continue;
        }
        let mut command = Vec::new();
        for arg in *args {
            if *arg == MODEL_PLACEHOLDER {
                let model = match model {
                    Some(model) => model.to_path_buf(),
                    None => match default_voice_model() {
                        Some(model) => model,
                        // A voice without a model is no voice at all.
                        None => continue,
                    },
                };
                command.push(model.to_string_lossy().to_string());
            } else {
                command.push(arg.to_string());
            }
        }
        // The extra arguments come last, so that they can change what the
        // engine was told to do.
        command.extend(options.iter().cloned());
        return Some((program.to_string(), command));
    }
    None
}

/// The voices that are preferred, in the order they are tried.
///
/// These are the voices of women: a voice of this kind is meant to sound
/// like a person, and the ones that were made for it are the ones that
/// manage it best.
const FEMALE_VOICES: &[&str] = &[
    "hfc_female",
    "female",
    "lessac",
    "amy",
    "alba",
    "kristin",
    "kathleen",
    "jenny",
    "susan",
    "cori",
    "fiona",
    "tessa",
    "vicki",
    "aria",
    "joanna",
    "salli",
    "nicole",
    "serena",
];

/// The qualities of a voice, in the order they are preferred.
///
/// A voice made at a higher quality sounds better, and costs more to
/// speak with.
const QUALITIES: &[&str] = &["high", "medium", "low", "x_low"];

/// Find a voice model in the places lia looks.
///
/// A woman's voice is preferred, and a voice of a higher quality than the
/// rest. When there is none of those, any voice at all will do.
pub fn default_voice_model() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local").join("share").join("lia").join("models"));
        roots.push(home.join(".local").join("share").join("piper-voices"));
        roots.push(home.join(".local").join("share").join("piper"));
        roots.push(home.join(".cache").join("piper"));
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        roots.push(PathBuf::from(&dir).join("lia").join("models"));
        roots.push(PathBuf::from(&dir).join("piper-voices"));
    }
    roots.push(PathBuf::from("/usr/share").join("piper-voices"));

    let mut best: Option<(u8, u8, PathBuf)> = None;
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "onnx") {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            // A voice of a woman comes first, and a voice of a higher
            // quality before a voice of a lower one.
            let woman = FEMALE_VOICES
                .iter()
                .position(|voice| name.contains(voice))
                .unwrap_or(FEMALE_VOICES.len()) as u8;
            let quality = QUALITIES
                .iter()
                .position(|quality| name.contains(quality))
                .unwrap_or(QUALITIES.len()) as u8;
            let better = match &best {
                Some((best_woman, best_quality, _)) => {
                    (woman, quality) < (*best_woman, *best_quality)
                }
                None => true,
            };
            if better {
                best = Some((woman, quality, path));
            }
        }
    }
    best.map(|(_, _, path)| path)
}

/// A player found on this machine.
#[derive(Debug, Clone)]
pub struct Player {
    /// The program that plays.
    pub program: String,
    /// The whole command line.
    pub command: Vec<String>,
}

impl Player {
    /// Whether the player is really there.
    pub fn exists(&self) -> bool {
        which(&self.program).is_some()
    }
}

/// The players, in the order they are tried.
///
/// Each of them is given the sound by the name `{file}`: a player that
/// plays a wav wants a file, and the ones that read raw samples do not
/// read a wav at all.
pub const PLAYERS: &[(&str, &[&str])] = &[
    ("gst-play-1.0", &["{file}"]),
    ("pw-play", &["{file}"]),
    ("ffplay", &["{file}"]),
    ("mpv", &["{file}"]),
    ("mplayer", &["{file}"]),
    ("cvlc", &["--play-and-exit", "{file}"]),
];

/// The placeholder in the arguments that stands for the sound file.
pub const SOUND_PLACEHOLDER: &str = "{file}";

/// Find a player.
pub fn player() -> Option<Player> {
    for (program, args) in PLAYERS {
        if which(program).is_none() {
            continue;
        }
        let mut command = vec![program.to_string()];
        command.extend(args.iter().map(|a| a.to_string()));
        return Some(Player {
            program: program.to_string(),
            command,
        });
    }
    None
}

/// What was found on this machine.
///
/// The doctor command prints this, so a missing program is obvious rather
/// than a surprise later.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    /// The recorder that will be used.
    pub recorder: Option<String>,
    /// The transcriber that will be used.
    pub transcriber: Option<String>,
    /// The speaker that will be used.
    pub speaker: Option<String>,
    /// The voice the speaker will speak with.
    pub voice_model: Option<String>,
    /// The player that will be used.
    pub player: Option<String>,
}

impl Capabilities {
    /// Look for everything.
    pub fn detect(model: Option<&Path>) -> Capabilities {
        let speaker = speak_wav_command(None, &[]).or_else(speak_command);
        let voice_model = speaker.as_ref().and_then(|(_, args)| model_name(args));
        Capabilities {
            recorder: record_command().map(|(program, _)| program),
            transcriber: transcribe_command(model).map(|(program, _)| program),
            speaker: speaker.map(|(program, _)| program),
            voice_model,
            player: player().map(|p| p.program),
        }
    }

    /// Whether lia can hear.
    pub fn can_listen(&self) -> bool {
        self.recorder.is_some() && self.transcriber.is_some()
    }

    /// Whether lia can speak.
    pub fn can_speak(&self) -> bool {
        self.speaker.is_some() || self.player.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search that finds every program, so that the commands can be
    /// looked at whatever is on this machine.
    fn everything(program: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/usr/bin").join(program))
    }

    /// A command is the program and its arguments, and the program is not
    /// among the arguments.
    ///
    /// A program that takes an argument of its own — a recorder takes the
    /// node to record from, a speaker takes the words to say — would take
    /// the name of the program instead, and do nothing at all.
    #[test]
    fn a_command_does_not_repeat_its_program() {
        let (program, args) = record_command_with(everything).expect("a recorder was found");
        assert_eq!(program, "pw-record");
        assert!(
            !args.iter().any(|arg| arg == &program),
            "the program is among its own arguments: {args:?}"
        );

        let (program, args) = transcribe_command_with(Some(Path::new("/model.bin")), everything)
            .expect("a transcriber was found");
        assert!(
            !args.iter().any(|arg| arg == &program),
            "the program is among its own arguments: {args:?}"
        );

        let (program, args) = speak_command_with(everything).expect("a speaker was found");
        assert!(
            !args.iter().any(|arg| arg == &program),
            "the program is among its own arguments: {args:?}"
        );
    }

    /// The first program that is there is the one that is used.
    #[test]
    fn the_first_program_that_is_there_is_used() {
        let (program, _) = record_command_with(everything).expect("a recorder was found");
        assert_eq!(program, RECORDERS[0].0);

        // A machine with nothing on it has no recorder at all.
        let none = |_: &str| None;
        assert!(record_command_with(none).is_none());
        assert!(speak_command_with(none).is_none());
    }
}
