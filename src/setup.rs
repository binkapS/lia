//! The setup wizard.
//!
//! `lia setup` stores provider API keys and messaging bot tokens in the
//! secrets file, so they need not be exported in every shell. The wizard is
//! deliberately plain: it asks for one thing at a time, keeps whatever is
//! already stored when the answer is blank, and never prints a secret back.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Result, bail};

use crate::config::{Config, ProviderKind};
use crate::secrets::{self, Secrets, describe_value};

/// One thing the wizard can ask for.
#[derive(Debug, Clone, Copy)]
struct Field<'a> {
    /// The environment variable it stands for.
    key: &'a str,
    /// What to call it on screen.
    label: &'static str,
    /// Whether the value must be hidden while it is typed, and masked when
    /// it is shown.
    secret: bool,
}

/// The channels the wizard knows how to set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Telegram,
    Discord,
    Slack,
    WhatsApp,
}

impl ChannelKind {
    /// The name a person types.
    pub fn name(self) -> &'static str {
        match self {
            ChannelKind::Telegram => "telegram",
            ChannelKind::Discord => "discord",
            ChannelKind::Slack => "slack",
            ChannelKind::WhatsApp => "whatsapp",
        }
    }

    /// The settings this channel needs.
    fn fields(self) -> &'static [Field<'static>] {
        match self {
            ChannelKind::Telegram => &[
                Field {
                    key: "TELEGRAM_BOT_TOKEN",
                    label: "bot token",
                    secret: true,
                },
                Field {
                    key: "TELEGRAM_ALLOWED_CHATS",
                    label: "allowed chats (comma separated)",
                    secret: false,
                },
            ],
            ChannelKind::Discord => &[
                Field {
                    key: "DISCORD_BOT_TOKEN",
                    label: "bot token",
                    secret: true,
                },
                Field {
                    key: "DISCORD_ALLOWED_CHANNELS",
                    label: "allowed channels (comma separated)",
                    secret: false,
                },
                Field {
                    key: "DISCORD_ALLOWED_USERS",
                    label: "allowed users (optional, comma separated)",
                    secret: false,
                },
            ],
            ChannelKind::Slack => &[
                Field {
                    key: "SLACK_BOT_TOKEN",
                    label: "bot token",
                    secret: true,
                },
                Field {
                    key: "SLACK_ALLOWED_CHANNELS",
                    label: "allowed channels (comma separated)",
                    secret: false,
                },
                Field {
                    key: "SLACK_ALLOWED_USERS",
                    label: "allowed users (optional, comma separated)",
                    secret: false,
                },
            ],
            ChannelKind::WhatsApp => &[
                Field {
                    key: "WHATSAPP_ALLOWED_NUMBERS",
                    label: "allowed numbers (optional, comma separated, with country code)",
                    secret: false,
                },
            ],
        }
    }
}

/// The secrets the setup screen knows by name. Anything else in the file is
/// still shown, at the end, so a key stored by hand is not a surprise.
const KNOWN: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "OpenAI API key"),
    ("ANTHROPIC_API_KEY", "Anthropic API key"),
    ("OLLAMA_API_KEY", "Ollama API key"),
    ("LIA_API_KEY", "generic provider key"),
    ("LIA_GATEWAY_TOKEN", "gateway token"),
    ("LIA_SPEAK_API_KEY", "speech endpoint key"),
    ("LIA_TRANSCRIBE_API_KEY", "transcription endpoint key"),
    ("TELEGRAM_BOT_TOKEN", "Telegram bot token"),
    ("TELEGRAM_ALLOWED_CHATS", "Telegram allowed chats"),
    ("DISCORD_BOT_TOKEN", "Discord bot token"),
    ("DISCORD_ALLOWED_CHANNELS", "Discord allowed channels"),
    ("DISCORD_ALLOWED_USERS", "Discord allowed users"),
    ("SLACK_BOT_TOKEN", "Slack bot token"),
    ("SLACK_ALLOWED_CHANNELS", "Slack allowed channels"),
    ("SLACK_ALLOWED_USERS", "Slack allowed users"),
    ("WHATSAPP_ALLOWED_NUMBERS", "WhatsApp allowed numbers"),
];

/// Run `lia setup`.
///
/// With `set`, `unset` or `show`, nothing is asked: the file is changed or
/// displayed and the command ends. Otherwise the wizard runs on stdin.
pub fn run(
    config: &Config,
    path: &Path,
    show: bool,
    set: &[String],
    unset: &[String],
) -> Result<()> {
    let mut secrets = Secrets::load(path)?;
    let mut changed = false;

    for assignment in set {
        let (name, value) = parse_assignment(assignment)?;
        secrets.set(&name, &value);
        changed = true;
    }
    for name in unset {
        if secrets.remove(name) {
            println!("removed {name}");
        } else {
            println!("{name} was not stored");
        }
        changed = true;
    }

    if changed {
        secrets.save(path)?;
        println!("wrote {}", path.display());
    }

    if show {
        show_stored(&secrets, path);
        return Ok(());
    }
    if changed {
        return Ok(());
    }

    wizard(config, path, secrets)
}

/// Print what is stored, with every secret masked.
pub fn show_stored(secrets: &Secrets, path: &Path) {
    println!("the stored secrets");
    println!("  {}", path.display());
    println!();

    for (key, label) in KNOWN {
        let status = status_of(secrets, key, true);
        if status != "not set" {
            println!("  {key:<28} {label} — {status}");
        }
    }

    let mut unknown: Vec<&str> = secrets
        .names()
        .filter(|name| !KNOWN.iter().any(|(known, _)| known == name))
        .collect();
    if !unknown.is_empty() {
        println!();
        println!("  other names");
        unknown.sort_unstable();
        for name in unknown {
            let status = status_of(secrets, name, true);
            println!("  {name:<28} {status}");
        }
    }

    println!();
    println!("An exported variable wins over a stored one.");
}

/// The interactive part.
fn wizard(config: &Config, path: &Path, mut secrets: Secrets) -> Result<()> {
    println!("lia setup");
    println!("  secrets are kept in {}", path.display());
    println!("  an exported variable still wins over a stored one");
    println!("  a blank answer keeps what is there; `-` removes it");
    println!();

    println!("the provider");
    println!(
        "  {} / {}",
        config.provider.as_str(),
        config.effective_model()
    );
    let key = provider_key(config.provider);
    ask_field(
        &mut secrets,
        Field {
            key,
            label: "API key",
            secret: true,
        },
    )?;

    for fallback in &config.fallback {
        let Some(name) = fallback.api_key_env.as_deref() else {
            continue;
        };
        if name.trim().is_empty() || name == key {
            continue;
        }
        ask_field(
            &mut secrets,
            Field {
                key: name,
                label: "API key for the fallback model",
                secret: true,
            },
        )?;
    }

    println!();
    println!("the channels");
    println!("  which: telegram, discord, slack, whatsapp (or `all`)");
    let Some(answer) = ask_line("  set up (comma separated, blank to finish): ")? else {
        return finish(&secrets, path);
    };
    if answer.trim().is_empty() {
        return finish(&secrets, path);
    }

    for channel in parse_channels(&answer)? {
        println!();
        println!("  {}", channel.name());
        for field in channel.fields() {
            ask_field(&mut secrets, *field)?;
        }
        if channel == ChannelKind::WhatsApp {
            println!("    the first start shows a qr code to scan from the phone;");
            println!("    the pairing is kept in the state directory afterwards");
        }
    }

    finish(&secrets, path)
}

/// Save, and say so, or say that nothing changed.
fn finish(secrets: &Secrets, path: &Path) -> Result<()> {
    println!();
    if secrets.is_empty() {
        println!("nothing was stored");
        return Ok(());
    }
    secrets.save(path)?;
    println!("wrote {}", path.display());
    println!("`lia serve` and the terminal will use these when no variable is exported.");
    Ok(())
}

/// Ask for one field, and store the answer.
fn ask_field(secrets: &mut Secrets, field: Field<'_>) -> Result<()> {
    let status = status_of(secrets, field.key, field.secret);
    let prompt = format!("  {} [{}]: ", field.label, status);
    let answer = if field.secret {
        ask_secret(&prompt)?
    } else {
        ask_line(&prompt)?
    };

    let Some(answer) = answer else {
        return Ok(());
    };
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(());
    }
    if answer == "-" {
        if secrets.remove(field.key) {
            println!("    removed {}", field.key);
        }
        return Ok(());
    }

    secrets.set(field.key, answer);
    Ok(())
}

/// What to show in brackets beside a field.
fn status_of(secrets: &Secrets, key: &str, secret: bool) -> String {
    if secrets::real_env(key).is_some() {
        return "set in the environment (it wins)".to_string();
    }
    let Some(value) = secrets.get(key) else {
        return "not set".to_string();
    };
    if secret {
        describe_value(value)
    } else {
        value.to_string()
    }
}

/// The provider's usual key variable.
pub fn provider_key(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "ANTHROPIC_API_KEY",
        ProviderKind::Openai => "OPENAI_API_KEY",
        ProviderKind::Ollama | ProviderKind::OllamaCloud => "OLLAMA_API_KEY",
        ProviderKind::LlamaCpp => "LLAMA_CPP_API_KEY",
    }
}

/// Read `NAME=VALUE`.
pub fn parse_assignment(text: &str) -> Result<(String, String)> {
    let Some((name, value)) = text.split_once('=') else {
        bail!("expected NAME=VALUE, not '{text}'");
    };
    let name = name.trim();
    if name.is_empty() {
        bail!("the name before '=' is empty");
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        bail!("'{name}' is not an environment variable name");
    }
    Ok((name.to_string(), value.trim().to_string()))
}

/// Read a channel selection such as `telegram, whatsapp` or `all`.
pub fn parse_channels(text: &str) -> Result<Vec<ChannelKind>> {
    let mut channels = Vec::new();
    for part in text.split(|character: char| character == ',' || character.is_whitespace()) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.eq_ignore_ascii_case("all") {
            for channel in [
                ChannelKind::Telegram,
                ChannelKind::Discord,
                ChannelKind::Slack,
                ChannelKind::WhatsApp,
            ] {
                if !channels.contains(&channel) {
                    channels.push(channel);
                }
            }
            continue;
        }
        let channel = match part.to_ascii_lowercase().as_str() {
            "telegram" => ChannelKind::Telegram,
            "discord" => ChannelKind::Discord,
            "slack" => ChannelKind::Slack,
            "whatsapp" => ChannelKind::WhatsApp,
            other => {
                bail!("unknown channel '{other}' (expected telegram, discord, slack or whatsapp)")
            }
        };
        if !channels.contains(&channel) {
            channels.push(channel);
        }
    }
    Ok(channels)
}

/// Ask for a line, without hiding it.
fn ask_line(prompt: &str) -> Result<Option<String>> {
    read_line(prompt, false)
}

/// Ask for a line, hiding what is typed when this is a terminal.
fn ask_secret(prompt: &str) -> Result<Option<String>> {
    read_line(prompt, true)
}

fn read_line(prompt: &str, hide: bool) -> Result<Option<String>> {
    print!("{prompt}");
    std::io::stdout().flush()?;

    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    let hidden = hide && terminal && set_echo(false);

    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);

    if hidden {
        set_echo(true);
        println!();
    }

    let read = read?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(
        line.trim_end_matches(['\r', '\n'])
            .to_string(),
    ))
}

/// Turn terminal echo off or on.
#[cfg(unix)]
fn set_echo(on: bool) -> bool {
    std::process::Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn set_echo(_on: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_assignment_is_read() {
        let (name, value) = parse_assignment("OPENAI_API_KEY=sk-test").expect("it is read");
        assert_eq!(name, "OPENAI_API_KEY");
        assert_eq!(value, "sk-test");
    }

    #[test]
    fn a_bad_assignment_is_refused() {
        assert!(parse_assignment("OPENAI_API_KEY").is_err());
        assert!(parse_assignment("=value").is_err());
        assert!(parse_assignment("NOT-A-NAME=value").is_err());
    }

    #[test]
    fn the_channels_are_named_once() {
        let channels = parse_channels("Telegram, telegram whatsapp").expect("they are read");
        assert_eq!(channels, vec![ChannelKind::Telegram, ChannelKind::WhatsApp]);
    }

    #[test]
    fn all_means_all_of_them() {
        let channels = parse_channels("all").expect("they are read");
        assert_eq!(channels.len(), 4);
    }

    #[test]
    fn an_unknown_channel_is_refused() {
        let error = parse_channels("telegram, myspace").expect_err("it is refused");
        assert!(error.to_string().contains("myspace"));
    }

    #[test]
    fn the_provider_has_its_own_variable() {
        assert_eq!(provider_key(ProviderKind::Openai), "OPENAI_API_KEY");
        assert_eq!(provider_key(ProviderKind::Anthropic), "ANTHROPIC_API_KEY");
        assert_eq!(provider_key(ProviderKind::Ollama), "OLLAMA_API_KEY");
        assert_eq!(provider_key(ProviderKind::OllamaCloud), "OLLAMA_API_KEY");
        assert_eq!(provider_key(ProviderKind::LlamaCpp), "LLAMA_CPP_API_KEY");
    }
}
