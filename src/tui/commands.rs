//! The commands the message box understands.
//!
//! The same list drives the preview that opens when a slash is typed and
//! the command section of the help overlay, so the two cannot drift apart.

/// One command that can be typed at the prompt.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// The command itself, including the slash.
    pub name: &'static str,
    /// The arguments it takes, for the help text.
    pub args: &'static str,
    /// What it does, in one line.
    pub description: &'static str,
}

/// Everything the prompt understands.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "/help",
        args: "",
        description: "show this command index and the key map",
    },
    CommandSpec {
        name: "/clear",
        args: "",
        description: "clear the transcript and start a fresh session",
    },
    CommandSpec {
        name: "/sessions",
        args: "",
        description: "switch to a saved session",
    },
    CommandSpec {
        name: "/model",
        args: "[name]",
        description: "show the model, or set it for this session",
    },
    CommandSpec {
        name: "/tools",
        args: "",
        description: "list the tools this assistant can call",
    },
    CommandSpec {
        name: "/skills",
        args: "",
        description: "list the skills that were found",
    },
    CommandSpec {
        name: "/image",
        args: "<path>",
        description: "attach a picture to the next message",
    },
    CommandSpec {
        name: "/listen",
        args: "",
        description: "record the microphone and send the words",
    },
    CommandSpec {
        name: "/dictate",
        args: "",
        description: "record the microphone and put the words in the box",
    },
    CommandSpec {
        name: "/speak",
        args: "<words>",
        description: "say a line out loud",
    },
    CommandSpec {
        name: "/voice",
        args: "on|off",
        description: "show or set whether replies are spoken",
    },
    CommandSpec {
        name: "/handsfree",
        args: "on|off",
        description: "listen, answer aloud, and listen again",
    },
    CommandSpec {
        name: "/live",
        args: "on|off|none|camera|screen",
        description: "talk over the answer, with the microphone always open",
    },
    CommandSpec {
        name: "/config",
        args: "[set key value]",
        description: "show or change a saved setting",
    },
    CommandSpec {
        name: "/quit",
        args: "",
        description: "leave the interface",
    },
];

/// The commands that match a prompt that is part way through a name.
///
/// A preview only belongs while the person is still naming the command:
/// once there is a space, the arguments are being typed, and the list gets
/// out of the way.
pub fn matching(text: &str) -> Vec<&'static CommandSpec> {
    let text = text.trim_start();
    if !text.starts_with('/') || text.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let typed = text.to_lowercase();
    COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(&typed))
        .collect()
}
