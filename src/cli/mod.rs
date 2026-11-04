//! Command-line entry point.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use crate::agent::session::SessionStore;
use crate::agent::{Agent, AgentConfig, NoInteraction, SessionRunner};
use crate::config::{Config, LiveShare, McpServerConfig, McpTransport, ProviderKind};
use crate::tool::ToolRegistry;
use crate::tool::fetch::FetchTool;
use crate::tool::filesystem::{
    CopyPathTool, EditFileTool, FormatFileTool, ListDirectoryTool, MovePathTool, PatchTool,
    ReadFileTool, SearchTool, Workspace, WriteFileTool,
};
use crate::tool::search::WebSearchTool;
use crate::tool::shell::ShellTool;
use crate::tool::task::TaskTool;
use crate::tool::todo::TodoTool;

/// A local-first AI assistant.
#[derive(Debug, Parser)]
#[command(name = "lia", version, about, long_about = None)]
pub struct Cli {
    /// Path to the config file.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// The workspace the assistant works in.
    #[arg(long, short, global = true, value_name = "DIR")]
    pub workspace: Option<PathBuf>,

    /// Where sessions and other state are kept.
    #[arg(long, global = true, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Override the provider (`openai` or `anthropic`).
    #[arg(long, global = true, value_name = "NAME")]
    pub provider: Option<String>,

    /// Override the model id.
    #[arg(long, short, global = true, value_name = "MODEL")]
    pub model: Option<String>,

    /// Override the provider's API root.
    #[arg(long, global = true, value_name = "URL")]
    pub base_url: Option<String>,

    /// Turn on debug logging.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// What to do.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the interactive terminal interface (default).
    Tui,

    /// Send one prompt and print the reply.
    Prompt {
        /// The prompt to send.
        text: String,
        /// Do not use tools; answer from the model alone.
        #[arg(long)]
        no_tools: bool,
        /// A picture to send along with the prompt. May be given more than once.
        #[arg(long, value_name = "PATH")]
        image: Vec<PathBuf>,
    },

    /// Read a prompt from stdin and answer it.
    Pipe {
        /// Do not use tools; answer from the model alone.
        #[arg(long)]
        no_tools: bool,
        /// A picture to send along with the prompt. May be given more than once.
        #[arg(long, value_name = "PATH")]
        image: Vec<PathBuf>,
    },

    /// Start the gateway daemon.
    Gateway {
        /// Address to bind.
        #[arg(long)]
        host: Option<String>,
        /// Port to listen on.
        #[arg(long, short)]
        port: Option<u16>,
        /// Shared secret clients must present.
        #[arg(long)]
        token: Option<String>,
    },

    /// Start the gateway and the messaging channels together.
    Serve {
        /// Address to bind.
        #[arg(long)]
        host: Option<String>,
        /// Port to listen on.
        #[arg(long, short)]
        port: Option<u16>,
        /// Shared secret clients must present.
        #[arg(long)]
        token: Option<String>,
    },

    /// List the tools the assistant can call.
    Tools,

    /// List the models the provider offers.
    Models,

    /// Show the resolved configuration.
    Config {
        /// Write the configuration to this path instead of printing it.
        #[arg(long)]
        write: Option<PathBuf>,
    },

    /// List stored sessions.
    Sessions,

    /// List the skills the assistant can use.
    Skills,

    /// Show the MCP servers and the tools they offer.
    Mcp,

    /// Run the file server: an MCP server that offers a few folders.
    ///
    /// This is what lia starts for itself when the configuration names
    /// folders for it. It speaks MCP over its standard streams.
    McpFiles {
        /// A folder the server may work in. May be given more than once.
        #[arg(long = "allow", value_name = "DIR")]
        allow: Vec<PathBuf>,

        /// Refuse to change anything; only read.
        #[arg(long)]
        read_only: bool,
    },

    /// Show the work the assistant has promised to do later.
    Schedule,

    /// Sort the notes of the days that have passed into the kept facts.
    Consolidate,

    /// Show what the assistant keeps between conversations.
    Memory {
        /// How many days of notes to show.
        #[arg(long, default_value = "3")]
        days: usize,
    },

    /// Keep a personal inbox outside conversation history.
    Inbox {
        #[command(subcommand)]
        command: InboxCommand,
    },

    /// Show today's calendar events and due inbox items.
    ///
    /// A read-only CalDAV calendar is included only when LIA_CALDAV_URL,
    /// LIA_CALDAV_USER and LIA_CALDAV_PASSWORD are set (LIA_CALDAV_CALENDAR
    /// names it in output; default "calendar").
    Agenda {
        /// IANA timezone that defines "today" (for example Europe/Paris).
        #[arg(long, default_value = "UTC")]
        timezone: String,
    },

    /// Read chosen email messages (read-only) and keep local drafts.
    ///
    /// Reading needs LIA_JMAP_SESSION_URL and LIA_JMAP_TOKEN (a JMAP API
    /// token, for example from Fastmail). Lia can only read the messages you
    /// name and can never send, move or delete mail.
    Mail {
        #[command(subcommand)]
        command: MailCommand,
    },

    /// Manage opt-in daily briefing routines delivered by `lia serve`.
    Routine {
        #[command(subcommand)]
        command: RoutineCommand,
    },

    /// Record from the microphone and print what was said.
    Listen {
        /// How long to record for, in seconds.
        #[arg(long, default_value = "30")]
        seconds: u64,
    },

    /// Say a line out loud.
    Speak {
        /// The words to say.
        text: String,
    },

    /// Talk with the assistant, with the microphone always open.
    ///
    /// Talking over the answer stops it, as in the Gemini app's live mode.
    Live {
        /// What to share besides the voice: "none", "camera" or "screen".
        #[arg(long, value_name = "WHAT")]
        share: Option<String>,
    },

    /// Check that everything is in place.
    Doctor,

    /// Store API keys and bot tokens, so they need not be exported.
    ///
    /// With no options it asks for the keys the configuration needs and for
    /// the channels to set up. A blank answer keeps what is stored.
    Setup {
        /// Show what is stored, with secrets masked.
        #[arg(long)]
        show: bool,
        /// Store a secret, as NAME=VALUE. May be given more than once.
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// Remove a stored secret. May be given more than once.
        #[arg(long = "unset", value_name = "NAME")]
        unset: Vec<String>,
    },

    /// Let people in who have asked to be let in.
    Pair {
        /// What to do: "list", "approve", "deny" or "revoke".
        #[arg(default_value = "list")]
        action: String,
        /// The code, when the action needs one.
        code: Option<String>,
    },
}

/// An inbox operation.
#[derive(Debug, Subcommand)]
pub enum InboxCommand {
    /// Capture a new item.
    Capture {
        /// The item to remember.
        text: String,
        /// What kind of record to create.
        #[arg(long, value_enum, default_value = "inbox")]
        kind: InboxKind,
        /// How important it is.
        #[arg(long, value_enum, default_value = "normal")]
        priority: InboxPriority,
        /// An RFC 3339 due time.
        #[arg(long)]
        due_at: Option<String>,
    },
    /// List records (archived records are hidden unless requested).
    List {
        #[arg(long, value_enum)]
        kind: Option<InboxKind>,
        #[arg(long, value_enum)]
        state: Option<InboxState>,
        #[arg(long)]
        include_archived: bool,
    },
    /// Show one record.
    Show { id: uuid::Uuid },
    /// Mark a record complete.
    Complete { id: uuid::Uuid },
    /// Defer a record until an RFC 3339 time.
    Defer {
        id: uuid::Uuid,
        #[arg(long)]
        until: String,
    },
    /// Archive a record.
    Archive { id: uuid::Uuid },
    /// Permanently delete a record.
    Delete { id: uuid::Uuid },
}

/// A mail operation. Only messages named by ID are ever read.
#[derive(Subcommand, Debug, Clone)]
pub enum MailCommand {
    /// Print the subject and body of the named messages.
    Read {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Save a local draft that cites a message. Nothing is sent.
    Draft {
        /// The message the draft is about.
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        text: String,
        /// Draft a reply instead of a task.
        #[arg(long)]
        reply: bool,
    },
    /// Send a local draft after showing exactly what will be sent and asking
    /// you to confirm. Needs an interactive terminal and a token that may send.
    Send {
        /// The draft to send.
        id: u64,
        /// The recipient address.
        #[arg(long)]
        to: String,
    },
    /// List local drafts.
    Drafts,
    /// Delete one local draft.
    DeleteDraft { id: u64 },
    /// Delete every local draft.
    Forget,
}

/// A routine operation. Routines belong to the local owner and deliver to
/// exactly one explicit chat.
#[derive(Debug, Subcommand)]
pub enum RoutineCommand {
    /// Create a routine (it runs only while `lia serve` is running).
    Create {
        /// A name for the routine.
        id: String,
        /// When it runs, for example "every day at 9:00".
        #[arg(long)]
        when: String,
        /// The IANA time zone the schedule is read in, for example Europe/Berlin.
        #[arg(long)]
        timezone: String,
        /// The channel to deliver through, for example telegram.
        #[arg(long)]
        channel: String,
        /// The one conversation in that channel to deliver to.
        #[arg(long)]
        conversation: String,
        /// Include due tasks and reminders.
        #[arg(long)]
        due_items: bool,
        /// Include scheduled jobs that run soon.
        #[arg(long)]
        upcoming_jobs: bool,
        /// Include your own notes.
        #[arg(long)]
        approved_notes: bool,
        /// Maximum briefing size in bytes.
        #[arg(long, default_value = "8192")]
        max_bytes: usize,
    },
    /// List routines and how their last delivery went.
    List,
    /// Stop a routine without forgetting it.
    Pause { id: String },
    /// Resume a paused routine from its next slot.
    Resume { id: String },
    /// Forget a routine.
    Remove { id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InboxKind {
    Inbox,
    Task,
    Note,
    Reminder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InboxPriority {
    Low,
    Normal,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InboxState {
    Open,
    Done,
    Archived,
}

/// Run the CLI.
pub async fn run(cli: Cli) -> Result<()> {
    // The stored secrets are read before anything asks for a key. A real
    // environment variable still wins over the file.
    let secrets_path = crate::secrets::path_for(cli.config.as_deref());
    crate::secrets::init(&secrets_path)?;

    let config = load_config(&cli)?;

    match cli.command {
        Some(Command::Tui) | None => {
            spawn_maintenance(&config);
            crate::tui::run(config).await
        }
        Some(Command::Prompt {
            text,
            no_tools,
            image,
        }) => one_shot(config, text, no_tools, false, image).await,
        Some(Command::Pipe { no_tools, image }) => {
            let text = read_stdin().await?;
            one_shot(config, text, no_tools, true, image).await
        }
        Some(Command::Gateway { host, port, token }) => {
            serve_gateway(config, host, port, token, false).await
        }
        Some(Command::Serve { host, port, token }) => {
            spawn_maintenance(&config);
            serve_gateway(config, host, port, token, true).await
        }
        Some(Command::Tools) => {
            let mut tools = build_tools(&config);
            let servers = mcp_servers(&config);
            if config.mcp_enabled && !servers.is_empty() {
                let registry = crate::mcp::McpRegistry::connect(&servers).await;
                for tool in registry.tools().await {
                    tools.register(tool);
                }
            }
            for name in tools.names() {
                if let Some(tool) = tools.get(&name) {
                    println!("{} — {}", tool.name(), tool.description());
                }
            }
            Ok(())
        }
        Some(Command::Config { write }) => {
            if let Some(path) = write {
                config.save(&path)?;
                eprintln!("wrote {}", path.display());
            } else {
                println!("{}", toml::to_string_pretty(&config)?);
            }
            Ok(())
        }
        Some(Command::Sessions) => {
            let store = SessionStore::open(config.state_dir.join("sessions"))?;
            for session in store.list() {
                println!(
                    "{}  {}  ({} messages, {})",
                    session.id,
                    session.title,
                    session.messages.len(),
                    session.updated_at.format("%Y-%m-%d %H:%M")
                );
            }
            Ok(())
        }
        Some(Command::Skills) => {
            let skills = config.discover_skills();
            if skills.is_empty() {
                println!("no skills found");
                println!();
                println!("Skills live in a directory with a SKILL.md file:");
                for (_, root) in config.skill_roots() {
                    println!("  {}", root.display());
                }
            } else {
                for skill in skills.iter() {
                    println!(
                        "{}  {}  ({})",
                        skill.name,
                        skill.description,
                        skill.file.display()
                    );
                }
            }
            Ok(())
        }
        Some(Command::McpFiles { allow, read_only }) => serve_file_server(allow, read_only).await,
        Some(Command::Mcp) => {
            let servers = mcp_servers(&config);
            if servers.is_empty() {
                println!("no MCP servers configured");
                println!();
                println!("Add one to the config file:");
                println!("  [mcp_servers.notes]");
                println!("  transport = \"stdio\"");
                println!("  command = \"mcp-server-filesystem\"");
                println!("  args = [\"/home/you\"]");
                println!();
                println!("Or name the folders lia may reach, and let it start");
                println!("a file server of its own:");
                println!("  [filesystem]");
                println!("  allow = [\"~/projects\", \"~/notes\"]");
                return Ok(());
            }

            let registry = crate::mcp::McpRegistry::connect(&servers).await;
            let tools = registry.tools().await;

            if tools.is_empty() {
                println!("no MCP server answered");
            }
            for tool in tools {
                let description = tool.description();
                if description.is_empty() {
                    println!("{}", tool.name());
                } else {
                    println!("{} — {description}", tool.name());
                }
            }
            Ok(())
        }
        Some(Command::Consolidate) => {
            let provider = config.build_provider()?;
            let memory = Arc::new(crate::memory::MemoryStore::open(
                config.state_dir.join("memory"),
                config.workspace.clone(),
            )?);
            let consolidator =
                crate::memory::consolidate::Consolidator::new(memory.clone(), provider);
            let summary = consolidator.run().await?;

            if summary.days == 0 {
                println!("there is nothing to sort out");
            } else {
                println!(
                    "looked over {} day(s), kept {} fact(s), left {} out",
                    summary.days, summary.kept, summary.left_out
                );
            }
            Ok(())
        }
        Some(Command::Pair { action, code }) => {
            let store = crate::channel::PairingStore::open(config.state_dir.join("pairing.json"))?;

            match action.as_str() {
                "approve" => {
                    let code = code.ok_or_else(|| anyhow::anyhow!("which code?"))?;
                    let grant = store.approve(&code).map_err(|e| anyhow::anyhow!(e))?;
                    println!("let in: {}", grant.describe());
                }
                "deny" => {
                    let code = code.ok_or_else(|| anyhow::anyhow!("which code?"))?;
                    if store.deny(&code) {
                        println!("refused: {code}");
                    } else {
                        println!("there is no request for {code}");
                    }
                }
                "revoke" => {
                    let code = code.ok_or_else(|| anyhow::anyhow!("which code?"))?;
                    if store.revoke(&code) {
                        println!("let out: {code}");
                    } else {
                        println!("nobody was let in with {code}");
                    }
                }
                _ => {
                    let requests = store.requests();
                    let grants = store.grants();
                    if requests.is_empty() && grants.is_empty() {
                        println!("nobody is waiting, and nobody is let in");
                        return Ok(());
                    }

                    if !requests.is_empty() {
                        println!("waiting:");
                        for request in &requests {
                            println!("  {}", request.describe());
                        }
                    }
                    if !grants.is_empty() {
                        if !requests.is_empty() {
                            println!();
                        }
                        println!("let in:");
                        for grant in &grants {
                            println!("  {}", grant.describe());
                        }
                    }
                }
            }
            Ok(())
        }
        Some(Command::Memory { days }) => {
            let memory = crate::memory::MemoryStore::open(
                config.state_dir.join("memory"),
                config.workspace.clone(),
            )?;

            println!("what is kept:");
            for kind in [
                crate::memory::MemoryKind::Core,
                crate::memory::MemoryKind::User,
            ] {
                let Some(path) = memory.curated_path(kind) else {
                    continue;
                };
                let name = match kind {
                    crate::memory::MemoryKind::Core => "the project",
                    crate::memory::MemoryKind::User => "the person",
                    _ => continue,
                };
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.trim().is_empty() {
                    println!("  {name}: nothing yet");
                } else {
                    println!("  {name}:");
                    for line in text.lines().filter(|l| l.starts_with("- ")) {
                        println!("    {line}");
                    }
                }
            }

            let all_days = memory.daily_files();
            let recent: Vec<_> = all_days.iter().rev().take(days.max(1)).collect();
            if recent.is_empty() {
                println!("\nthere are no notes yet");
            } else {
                println!("\nthe notes of the last day(s):");
                for (day, _) in recent {
                    let notes = memory.notes_of(*day);
                    println!("  {day}: {} note(s)", notes.len());
                    for note in notes.iter().take(5) {
                        println!("    [{}] {}", note.origin.as_str(), note.text);
                    }
                    if notes.len() > 5 {
                        println!("    … and {} more", notes.len() - 5);
                    }
                }
            }
            Ok(())
        }
        Some(Command::Inbox { command }) => run_inbox(&config.state_dir, command),
        Some(Command::Mail { command }) => run_mail(&config.state_dir, command).await,
        Some(Command::Agenda { timezone }) => {
            run_agenda(&config.state_dir, &timezone, chrono::Utc::now()).await
        }
        Some(Command::Routine { command }) => {
            run_routine(&config.state_dir, command, chrono::Utc::now())
        }
        Some(Command::Listen { seconds }) => listen(config, seconds).await,
        Some(Command::Speak { text }) => speak(config, text).await,
        Some(Command::Live { share }) => live(config, share).await,
        Some(Command::Setup { show, set, unset }) => {
            let path = crate::secrets::path_for(config.config_path.as_deref());
            crate::setup::run(&config, &path, show, &set, &unset)
        }
        Some(Command::Doctor) => doctor(config).await,
        Some(Command::Models) => models(config).await,
        Some(Command::Schedule) => {
            let store = crate::schedule::JobStore::open(config.state_dir.join("schedule"))?;
            let jobs = store.list();
            if jobs.is_empty() {
                println!("nothing is scheduled");
                return Ok(());
            }
            for job in jobs {
                println!("{}", job.describe());
                if let Some(last) = job.last_run {
                    println!("  last ran: {last}");
                }
            }
            Ok(())
        }
    }
}

async fn run_mail(state_dir: &Path, command: MailCommand) -> Result<()> {
    use crate::email::{DraftKind, DraftStore, MailScope};

    let path = state_dir.join("mail").join("drafts.json");
    let load = || {
        if path.exists() {
            DraftStore::load(&path)
        } else {
            Ok(DraftStore::new())
        }
    };
    let save = |store: &DraftStore| -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(store.save(&path)?)
    };
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    match command {
        MailCommand::Read { ids } => {
            let (Some(session_url), Some(token)) = (
                crate::secrets::var("LIA_JMAP_SESSION_URL"),
                crate::secrets::var("LIA_JMAP_TOKEN"),
            ) else {
                anyhow::bail!("set LIA_JMAP_SESSION_URL and LIA_JMAP_TOKEN to read mail");
            };
            let scope = MailScope::new(ids)?;
            let client =
                crate::jmap::JmapClient::new(crate::jmap::JmapConfig { session_url, token })?;
            for message in client.fetch_scoped(&scope).await? {
                println!("--- message {} (untrusted content) ---", message.source_id);
                println!("Subject: {}", message.subject.expose());
                println!("{}", message.body.expose());
            }
        }
        MailCommand::Draft {
            id,
            title,
            text,
            reply,
        } => {
            let mut store = load()?;
            let scope = MailScope::new([id.clone()])?;
            let kind = if reply {
                DraftKind::Reply
            } else {
                DraftKind::Task
            };
            let draft = store.propose(&scope, kind, &[id.as_str()], &title, &text, now)?;
            save(&store)?;
            println!("saved draft {draft} (local only; nothing is sent)");
        }
        MailCommand::Send { id, to } => {
            use std::io::{BufRead, IsTerminal, Write};

            let (Some(session_url), Some(token)) = (
                crate::secrets::var("LIA_JMAP_SESSION_URL"),
                crate::secrets::var("LIA_JMAP_TOKEN"),
            ) else {
                anyhow::bail!("set LIA_JMAP_SESSION_URL and LIA_JMAP_TOKEN to send mail");
            };
            let interactive = std::io::stdin().is_terminal();
            let store = load()?;
            let draft = store
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("no draft {id}"))?;
            let action = crate::external_action::ExternalAction {
                owner: crate::inbox_identity::LOCAL_OWNER_ID.to_owned(),
                account: session_url.clone(),
                target: to.clone(),
                payload: format!("{}\n\n{}", draft.title, draft.text),
                side_effects: "sends one email that cannot be recalled".to_owned(),
            };
            let gate = crate::external_action::ExternalActionGate::new(
                std::time::Duration::from_secs(300),
            );
            let now = std::time::SystemTime::now();
            let preview = gate.preview(&action, interactive, now)?;
            println!(
                "To: {}\nSubject: {}\n\n{}",
                preview.target, draft.title, draft.text
            );
            println!("\n{}", preview.side_effects);
            print!("Send this email? [y/N] ");
            std::io::stdout().flush()?;
            let mut answer = String::new();
            std::io::stdin().lock().read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                println!("not sent");
                return Ok(());
            }
            gate.confirm(
                &preview.id,
                &action,
                interactive,
                std::time::SystemTime::now(),
            )?;
            let client =
                crate::jmap::JmapClient::new(crate::jmap::JmapConfig { session_url, token })?;
            let from = client.send_plain(&to, &draft.title, &draft.text).await?;
            println!("sent from {from} to {to}");
        }
        MailCommand::Drafts => {
            let store = load()?;
            if store.list().is_empty() {
                println!("no drafts");
            }
            for draft in store.list() {
                println!(
                    "{} [{:?}/{:?}] {} (from {})",
                    draft.id,
                    draft.kind,
                    draft.status,
                    draft.title,
                    draft.source_ids.join(", ")
                );
                println!("  {}", draft.text);
            }
        }
        MailCommand::DeleteDraft { id } => {
            let mut store = load()?;
            store.delete(id)?;
            save(&store)?;
            println!("deleted draft {id}");
        }
        MailCommand::Forget => {
            let mut store = load()?;
            store.disconnect();
            save(&store)?;
            println!("deleted every draft");
        }
    }
    Ok(())
}

async fn run_agenda(
    state_dir: &Path,
    timezone: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    println!(
        "{}",
        crate::agenda::agenda_text(state_dir, timezone, now).await?
    );
    Ok(())
}

fn run_inbox(state_dir: &Path, command: InboxCommand) -> Result<()> {
    use crate::inbox::{Filter, InboxStore, Kind, NewRecord, Priority, Provenance, State, Update};

    let owner = crate::inbox::Owner::new(crate::inbox_identity::LOCAL_OWNER_ID)?;
    let mut store = InboxStore::open(state_dir.join("inbox"))?;
    match command {
        InboxCommand::Capture {
            text,
            kind,
            priority,
            due_at,
        } => {
            let mut record = NewRecord::new(
                match kind {
                    InboxKind::Inbox => Kind::Inbox,
                    InboxKind::Task => Kind::Task,
                    InboxKind::Note => Kind::Note,
                    InboxKind::Reminder => Kind::Reminder,
                },
                text,
                Provenance::User,
            );
            record.priority = match priority {
                InboxPriority::Low => Priority::Low,
                InboxPriority::Normal => Priority::Normal,
                InboxPriority::High => Priority::High,
            };
            record.due_at = due_at.as_deref().map(parse_inbox_time).transpose()?;
            let record = store.create(&owner, record)?;
            print_inbox_record(&record);
        }
        InboxCommand::List {
            kind,
            state,
            include_archived,
        } => {
            let filter = Filter {
                kind: kind.map(|kind| match kind {
                    InboxKind::Inbox => Kind::Inbox,
                    InboxKind::Task => Kind::Task,
                    InboxKind::Note => Kind::Note,
                    InboxKind::Reminder => Kind::Reminder,
                }),
                state: state.map(|state| match state {
                    InboxState::Open => State::Open,
                    InboxState::Done => State::Done,
                    InboxState::Archived => State::Archived,
                }),
                include_archived,
            };
            let records = store.list(&owner, &filter);
            if records.is_empty() {
                println!("nothing in the inbox");
            } else {
                for record in records {
                    print_inbox_record(&record);
                }
            }
        }
        InboxCommand::Show { id } => print_inbox_record(&store.get(&owner, id)?),
        InboxCommand::Complete { id } => {
            let record = store.update(
                &owner,
                id,
                Update {
                    state: Some(State::Done),
                    ..Update::default()
                },
            )?;
            print_inbox_record(&record);
        }
        InboxCommand::Defer { id, until } => {
            let record = store.update(
                &owner,
                id,
                Update {
                    snoozed_until: Some(Some(parse_inbox_time(&until)?)),
                    ..Update::default()
                },
            )?;
            print_inbox_record(&record);
        }
        InboxCommand::Archive { id } => {
            let record = store.update(
                &owner,
                id,
                Update {
                    state: Some(State::Archived),
                    ..Update::default()
                },
            )?;
            print_inbox_record(&record);
        }
        InboxCommand::Delete { id } => {
            store.delete(&owner, id)?;
            println!("deleted {id}");
        }
    }
    Ok(())
}

fn parse_inbox_time(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    Ok(chrono::DateTime::parse_from_rfc3339(value)?.to_utc())
}

fn print_inbox_record(record: &crate::inbox::Record) {
    let due = record
        .due_at
        .map(|due| format!("  (due {})", due.to_rfc3339()))
        .unwrap_or_default();
    let deferred = record
        .snoozed_until
        .map(|until| format!("  (deferred until {})", until.to_rfc3339()))
        .unwrap_or_default();
    println!(
        "{}  [{:?}/{:?}/{:?}]  {}{}{}",
        record.id, record.kind, record.state, record.priority, record.text, due, deferred
    );
}

/// Check that everything is in place.
///
/// The answer is for a person: what the assistant is set up with, whether
/// the model can be reached, which channels are watching, and whether the
/// commands will be confined. Nothing here prints a secret.
async fn doctor(config: Config) -> Result<()> {
    println!("the setup");
    println!(
        "  model: {}/{}",
        config.provider.as_str(),
        config.effective_model()
    );
    // The address the requests really go to, which for Ollama is the root
    // of the API rather than the path people are used to typing.
    let endpoint = match (&config.base_url, config.provider) {
        (Some(base_url), ProviderKind::Ollama | ProviderKind::OllamaCloud) => {
            crate::provider::ollama::api_root(base_url).to_string()
        }
        (Some(base_url), _) => base_url.clone(),
        (None, kind) => kind.default_base_url().unwrap_or_default().to_string(),
    };
    if !endpoint.is_empty() {
        println!("  endpoint: {endpoint}");
    }
    if !config.fallback.is_empty() {
        let names: Vec<String> = config
            .fallback
            .iter()
            .map(|f| format!("{}/{}", f.provider.as_str(), f.model))
            .collect();
        println!("  fallbacks: {}", names.join(", "));
    }
    println!("  workspace: {}", config.workspace.display());
    println!("  state: {}", config.state_dir.display());

    match std::fs::metadata(&config.workspace) {
        Ok(meta) if meta.is_dir() => println!("  the workspace is there"),
        Ok(_) => println!("  the workspace is not a directory"),
        Err(e) => println!("  the workspace cannot be read: {e}"),
    }

    match std::fs::create_dir_all(&config.state_dir) {
        Ok(()) => println!("  the state directory is ready"),
        Err(e) => println!("  the state directory cannot be made: {e}"),
    }

    if !config.filesystem.allow.is_empty() {
        println!("  the file server may work in:");
        for folder in &config.filesystem.allow {
            let path = crate::tool::filesystem::expand_tilde(
                &folder.to_string_lossy(),
                dirs::home_dir().as_deref(),
            );
            if path.is_dir() {
                println!("    {} — it is there", path.display());
            } else {
                println!("    {} — it is not there", path.display());
            }
        }
    }

    println!("the model");
    match config.build_provider() {
        Ok(provider) => match provider.list_models().await {
            Ok(models) if !models.is_empty() => {
                println!("  the provider offers {} model(s)", models.len())
            }
            Ok(_) => println!("  the provider offers no models"),
            Err(e) => {
                // The provider's own words, but only the first line of them.
                let reason = e.to_string();
                let reason = reason.lines().next().unwrap_or("").trim();
                let reason = reason.split('{').next().unwrap_or("").trim();
                let reason = reason.trim_end_matches(':').trim();
                println!("  the provider could not be reached: {reason}");
            }
        },
        Err(e) => println!("  it is not set up: {e}"),
    }

    println!("the channels");
    let channels = crate::channel::channels_from_env(None, &config.state_dir);
    if channels.is_empty() {
        println!("  none is watching — the terminal is the only way in");
    } else {
        for channel in channels {
            println!("  {} is watching", channel.name());
        }
    }

    println!("the voice");
    if !config.voice.enabled {
        println!("  it is switched off");
    } else {
        let voice = crate::voice::Voice::new(config.voice.clone());
        let capabilities = voice.capabilities();
        match &capabilities.recorder {
            Some(program) => println!("  the microphone is recorded by {program}"),
            None => println!("  no recorder was found — nothing can hear"),
        }
        match &capabilities.transcriber {
            Some(program) => println!("  the words come from {program}"),
            None => println!("  no transcriber was found — nothing can be heard"),
        }
        match &capabilities.speaker {
            Some(program) => match &capabilities.voice_model {
                Some(model) => println!("  the voice is {program} ({model})"),
                None => println!("  the voice is {program}"),
            },
            None => println!("  no speaker was found — nothing can be said"),
        }
        if !capabilities.can_listen() {
            println!("  install whisper.cpp, or point `voice.transcribe_url` at an endpoint");
        }
        if !voice.can_speak() && voice.can_synthesize() {
            println!(
                "  speech can be made, but no player was found to hear it — install one or set `voice.play_command`"
            );
        }
    }

    println!("the commands");
    if !config.sandbox.enabled {
        println!("  they are not confined — the sandbox is switched off");
    } else {
        let workspace =
            std::fs::canonicalize(&config.workspace).unwrap_or_else(|_| config.workspace.clone());
        let sandbox = crate::tool::sandbox::Sandbox::new(config.sandbox.clone(), workspace);
        if sandbox.available().await {
            println!("  they run in a sandbox");
        } else {
            println!("  there is no sandbox here, so they run as they did before");
        }
    }

    Ok(())
}

/// Record from the microphone and print what was said.
async fn listen(config: Config, seconds: u64) -> Result<()> {
    if !config.voice.enabled {
        anyhow::bail!("the voice is switched off in the configuration");
    }
    let voice = crate::voice::Voice::new(config.voice.clone());
    if !voice.can_listen() {
        anyhow::bail!("nothing on this machine can hear: a recorder and a transcriber are needed");
    }

    eprintln!("listening — press ctrl-c to stop");
    let text = voice
        .listen(crate::voice::RecordOptions {
            max_seconds: seconds.clamp(1, 600),
            silence: None,
        })
        .await?;
    if text.is_empty() {
        println!("nothing was heard");
    } else {
        println!("{text}");
    }
    Ok(())
}

/// Say a line out loud.
async fn speak(config: Config, text: String) -> Result<()> {
    if !config.voice.enabled {
        anyhow::bail!("the voice is switched off in the configuration");
    }
    let voice = crate::voice::Voice::new(config.voice.clone());
    if !voice.can_speak() {
        if voice.can_synthesize() {
            anyhow::bail!(
                "speech can be made, but there is no player to hear it: install one or set `voice.play_command`"
            );
        }
        anyhow::bail!("nothing on this machine can speak");
    }
    voice.say(&text).await?;
    Ok(())
}

/// Talk with the assistant, with the microphone always open.
///
/// This is live mode at the command line: no wake word, the answer is said
/// aloud, and the microphone stays open while it is being said. Talking over
/// her stops the answer, and what was said becomes the next question. The
/// words she hears from her own speaker are recognised by
/// [`crate::live::looks_like_echo`] and passed over.
async fn live(config: Config, share: Option<String>) -> Result<()> {
    if !config.voice.enabled {
        anyhow::bail!("the voice is switched off in the configuration");
    }
    let share = match share {
        Some(text) => text.parse::<LiveShare>().map_err(|e| anyhow::anyhow!(e))?,
        None => config.live.share,
    };
    let voice = Arc::new(crate::voice::Voice::new(config.voice.clone()));
    if !voice.can_listen() {
        anyhow::bail!("nothing on this machine can hear: a recorder and a transcriber are needed");
    }
    if !voice.can_speak() {
        if voice.can_synthesize() {
            anyhow::bail!(
                "speech can be made, but there is no player to hear it: install one or set `voice.play_command`"
            );
        }
        anyhow::bail!("nothing on this machine can speak");
    }

    let agent = build_agent(&config, config.tools_enabled, None, None).await?;
    let sessions = Arc::new(tokio::sync::Mutex::new(SessionStore::open(
        config.state_dir.join("sessions"),
    )?));
    let runner = SessionRunner::new(agent, sessions);
    let session_id = format!("live-{}", uuid::Uuid::new_v4());
    runner
        .set_approver(&session_id, Arc::new(NoInteraction))
        .await;

    let max_seconds = config.voice.max_seconds.clamp(1, 600);
    let silence = Duration::from_millis(config.voice.silence_ms.unwrap_or(1500));
    let similarity = config.live.echo_similarity;
    let mut spoken: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    // What was said over the answer, which becomes the next question.
    let mut next: Option<String> = None;

    eprintln!(
        "live — speak; ctrl-c to stop{}",
        if share == LiveShare::None {
            String::new()
        } else {
            format!(" · sharing the {}", share.as_str())
        }
    );

    loop {
        let text = match next.take() {
            Some(text) => text,
            None => {
                let options = crate::voice::RecordOptions {
                    max_seconds,
                    silence: Some(silence),
                };
                // The sender has to outlive the recording: the sound ends
                // when it is dropped.
                let (stop, _seen) = tokio::sync::watch::channel(false);
                let heard = tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    result = voice.listen_with(options, stop, |_| {}) => result?,
                };
                heard.trim().to_string()
            }
        };
        if text.is_empty() {
            continue;
        }
        let recent: Vec<String> = spoken.iter().cloned().collect();
        if crate::live::looks_like_echo(&text, &recent, similarity) {
            // Her own voice came back through the room.
            continue;
        }
        eprintln!("you: {text}");

        let mut pictures = Vec::new();
        if share != LiveShare::None {
            match crate::live::capture_frame(&config.live, share).await {
                Ok(image) => pictures.push(crate::provider::types::ContentBlock::Image {
                    media_type: image.media_type,
                    data: image.data,
                }),
                Err(e) => eprintln!("the picture could not be taken: {e}"),
            }
        }

        let message = if pictures.is_empty() {
            crate::provider::types::Message::user(text)
        } else {
            crate::provider::types::Message::user_with_images(text, pictures)
        };
        let answer = answer_turn(&runner, &session_id, message).await;
        if answer.trim().is_empty() {
            continue;
        }

        // A note of what was said, so the open microphone does not answer
        // her own voice.
        spoken.push_back(answer.clone());
        while spoken.len() > 16 {
            spoken.pop_front();
        }

        // The answer is said while the microphone is open. If the person
        // talks over it, the speaking is dropped and their words are the
        // next question.
        let options = crate::voice::RecordOptions {
            max_seconds,
            silence: Some(silence),
        };
        let (stop, _seen) = tokio::sync::watch::channel(false);
        let listening = voice.listen_with(options, stop, |_| {});
        tokio::pin!(listening);
        let saying = voice.say(&answer);
        tokio::pin!(saying);

        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = &mut saying => {
                result?;
            }
            result = &mut listening => {
                let heard = result?.trim().to_string();
                let recent: Vec<String> = spoken.iter().cloned().collect();
                if heard.is_empty() || crate::live::looks_like_echo(&heard, &recent, similarity) {
                    // Silence, or her own voice: the answer goes on.
                    saying.await?;
                } else {
                    next = Some(heard);
                }
            }
        }
    }

    Ok(())
}

/// Run one turn and print it, giving back the answer as one piece.
async fn answer_turn(
    runner: &Arc<SessionRunner>,
    session_id: &str,
    message: crate::provider::types::Message,
) -> String {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<crate::agent::AgentEvent>(256);
    let runner = runner.clone();
    let id = session_id.to_string();
    let handle = tokio::spawn(async move {
        runner.run_message(&id, Some(message), event_tx).await;
    });

    let mut answer = String::new();
    while let Some(event) = event_rx.recv().await {
        match event {
            crate::agent::AgentEvent::TextDelta(text) => {
                print!("{text}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                answer.push_str(&text);
            }
            crate::agent::AgentEvent::ToolCallStarted { name, .. } => {
                eprintln!("· {name}");
            }
            crate::agent::AgentEvent::Error(message) => {
                eprintln!("error: {message}");
            }
            crate::agent::AgentEvent::Notice(message) => {
                eprintln!("note: {message}");
            }
            _ => {}
        }
    }
    let _ = handle.await;
    if !answer.is_empty() {
        println!();
    }
    answer
}

/// List the models the provider offers.
///
/// The names are the ones to put in the configuration. Ollama's cloud
/// names a model one way in the app and another in the API, and this is
/// where the API's way is to be found.
async fn models(config: Config) -> Result<()> {
    let provider = config.build_provider()?;
    let models = provider.list_models().await?;
    if models.is_empty() {
        println!("the provider offers no models");
        return Ok(());
    }
    for model in models {
        println!("{}", model.id);
    }
    Ok(())
}

/// Load the configuration and apply command-line overrides.
fn load_config(cli: &Cli) -> Result<Config> {
    let path = cli.config.clone().unwrap_or_else(Config::default_path);
    let mut config = Config::load(&path)?;

    if let Some(workspace) = &cli.workspace {
        config.workspace = workspace.clone();
    }
    if let Some(state_dir) = &cli.state_dir {
        config.state_dir = state_dir.clone();
    }
    if let Some(provider) = &cli.provider {
        config.provider = provider
            .parse::<ProviderKind>()
            .map_err(anyhow::Error::msg)?;
    }
    if let Some(model) = &cli.model {
        config.model = Some(model.clone());
    }
    if let Some(base_url) = &cli.base_url {
        config.base_url = Some(base_url.clone());
    }
    if cli.verbose {
        config.log_level = "debug".into();
    }

    Ok(config)
}

/// The MCP servers lia should connect to.
///
/// The file server is not written in the config file: lia starts it itself,
/// from the folders the config names.
pub fn mcp_servers(config: &Config) -> HashMap<String, McpServerConfig> {
    let mut servers = config.mcp_servers.clone();
    if config.filesystem.allow.is_empty() {
        return servers;
    }
    // A server of the same name would offer tools under the same names, and
    // one of the two would quietly hide the other. The one that was asked
    // for by name keeps the name.
    if servers.contains_key("files") {
        tracing::warn!(
            "the MCP server 'files' is already configured, so lia's own file server is not started"
        );
        return servers;
    }
    servers.insert("files".to_string(), file_server(config));
    servers
}

/// The file server, as a server lia starts itself.
///
/// It is lia's own program, so the path of the running program is used:
/// there is nothing to install and nothing to find on the PATH.
fn file_server(config: &Config) -> McpServerConfig {
    let program = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "lia".to_string());

    let mut args = vec!["mcp-files".to_string()];
    for folder in &config.filesystem.allow {
        let path = crate::tool::filesystem::expand_tilde(
            &folder.to_string_lossy(),
            dirs::home_dir().as_deref(),
        );
        if !path.is_dir() {
            tracing::warn!(
                "the folder '{}' of the file server is not there, so the server will not start",
                path.display()
            );
        }
        args.push("--allow".to_string());
        args.push(folder.to_string_lossy().into_owned());
    }
    if config.filesystem.read_only {
        args.push("--read-only".to_string());
    }

    McpServerConfig {
        enabled: true,
        transport: McpTransport::Stdio,
        command: Some(program),
        args,
        ..Default::default()
    }
}

/// Run the file server: an MCP server that offers a few folders.
///
/// The server speaks MCP over its standard streams, one message per line.
/// Nothing else may be written to them.
async fn serve_file_server(folders: Vec<PathBuf>, read_only: bool) -> Result<()> {
    if folders.is_empty() {
        anyhow::bail!("give at least one folder with --allow");
    }
    let server = if read_only {
        crate::mcp::server::FileServer::read_only(folders)?
    } else {
        crate::mcp::server::FileServer::new(folders)?
    };
    crate::mcp::server::serve_stdio(server).await
}

/// Build the tool registry for a configuration.
pub fn build_tools(config: &Config) -> ToolRegistry {
    let mut workspace = Workspace::new(&config.workspace);
    workspace.allow_outside(!config.extra_allowed_paths.is_empty());

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new(workspace.clone())));
    registry.register(Arc::new(WriteFileTool::new(workspace.clone())));
    registry.register(Arc::new(crate::tool::document::DocumentReadTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(crate::tool::document::DocumentCreateTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(crate::tool::document::DocumentEditTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(EditFileTool::new(workspace.clone())));
    registry.register(Arc::new(ListDirectoryTool::new(workspace.clone())));
    registry.register(Arc::new(SearchTool::new(workspace.clone())));
    registry.register(Arc::new(CopyPathTool::new(workspace.clone())));
    registry.register(Arc::new(MovePathTool::new(workspace.clone())));
    registry.register(Arc::new(PatchTool::new(workspace.clone())));
    registry.register(Arc::new(FormatFileTool::new(workspace.clone())));
    registry.register(Arc::new(crate::tool::filesystem::GrepCodebaseTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(crate::tool::filesystem::FindSymbolsTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(crate::tool::filesystem::ReadUrlChunkedTool::new()));
    // The sandbox needs the workspace as an absolute path, so it can be
    // bound inside.
    let workspace =
        std::fs::canonicalize(&config.workspace).unwrap_or_else(|_| config.workspace.clone());
    let mut shell = ShellTool::new(config.workspace.to_string_lossy().to_string())
        .with_sandbox(crate::tool::sandbox::Sandbox::new(
            config.sandbox.clone(),
            workspace,
        ))
        .with_timeout(std::time::Duration::from_secs(config.shell.timeout_seconds))
        .with_allowed(config.shell.allowed.clone());
    for denied in &config.shell.denied {
        shell = shell.block(denied.clone());
    }
    registry.register(Arc::new(shell));

    registry.register(Arc::new(FetchTool::new()));
    registry.register(Arc::new(web_search_tool(config)));
    // The machine itself is a source the assistant can read: the same
    // readings the browser monitor draws, offered as tools.
    registry.register(Arc::new(crate::tool::system::SystemHealthTool));
    registry.register(Arc::new(crate::tool::system::ProcessListTool));
    registry.register(Arc::new(crate::tool::system::NetworkStatusTool));
    registry.register(Arc::new(crate::tool::system::DiagnosticsTool));
    registry.register(Arc::new(crate::tool::system::SecurityStatusTool::new(
        crate::tool::system::SecurityFacts::from_config(config),
    )));
    registry.register(Arc::new(crate::tool::system::OpenAppTool));
    registry.register(Arc::new(todo_tool(config)));
    registry.register(Arc::new(TaskTool::new()));
    if config.voice.enabled {
        let voice = Arc::new(crate::voice::Voice::new(config.voice.clone()));
        if voice.can_listen() {
            registry.register(Arc::new(
                crate::tool::voice::ListenTool::new(voice.clone())
                    .with_max_seconds(config.voice.max_seconds),
            ));
        }
        if voice.can_speak() {
            registry.register(Arc::new(crate::tool::voice::SpeakTool::new(voice.clone())));
        }
        registry.register(Arc::new(crate::tool::voice::VoiceHealthTool::new(voice)));
    }
    registry
}

/// The tool that keeps the list of things to do.
///
/// The lists are kept beside the sessions, so a plan survives a restart.
/// If the state directory cannot be used, the list is kept in memory for
/// this run, which is better than not having one at all.
fn todo_tool(config: &Config) -> TodoTool {
    TodoTool::with_dir(config.state_dir.join("todos")).unwrap_or_else(|_| TodoTool::new())
}

/// The tool that looks things up on the web.
///
/// The default engine needs no key. A configured endpoint is asked instead,
/// so the searches can be kept to one's own machine.
fn web_search_tool(config: &Config) -> WebSearchTool {
    let mut tool = WebSearchTool::new().with_count(config.search.count);
    if let Some(endpoint) = &config.search.endpoint {
        tool = tool.with_json_endpoint(endpoint);
    }
    tool
}

/// Build the agent described by a configuration.
///
/// `tools_enabled` decides whether the model may call tools at all. Skills
/// are part of that: with tools off, the model gets no catalogue either.
///
/// `schedule` is where the scheduler will be found. The agent is needed to
/// build the scheduler, so the tool is given a place to look rather than
/// the scheduler itself. Without one, the assistant cannot promise to do
/// something later, and the tool is not offered.
pub async fn build_agent(
    config: &Config,
    tools_enabled: bool,
    schedule: Option<crate::tool::schedule::SchedulerSlot>,
    messaging: Option<crate::channel::ChannelHub>,
) -> Result<Arc<Agent>> {
    let provider = config.build_provider()?;
    let mut registry = build_tools(config);

    if let Some(slot) = schedule {
        registry.register(Arc::new(crate::tool::schedule::ScheduleTool::new(slot)));
    }

    // The channels are what a message the assistant starts on its own is
    // sent through. The hub is filled later, when the channels have been
    // built, the way the scheduler arrives later than its slot.
    if let Some(hub) = messaging {
        registry.register(Arc::new(crate::tool::message::SendMessageTool::new(hub)));
    }

    // Skills are discovered once, here, so the catalogue in the system
    // prompt and the tool the model calls cannot disagree.
    let skills = if tools_enabled && config.tools_enabled {
        Arc::new(config.discover_skills())
    } else {
        Arc::new(crate::skill::SkillSet::default())
    };

    if !skills.is_empty() {
        registry.register(Arc::new(crate::skill::SkillTool::new(skills.clone())));
    }

    // The memory is shared between the tools and the prompt, so what the
    // model can read is what the model was told about.
    let memory = Arc::new(crate::memory::MemoryStore::open(
        config.state_dir.join("memory"),
        config.workspace.clone(),
    )?);

    if tools_enabled && config.tools_enabled {
        registry.register(Arc::new(crate::tool::memory::MemorySearchTool::new(
            memory.clone(),
        )));
        registry.register(Arc::new(crate::tool::memory::MemoryGetTool::new(
            memory.clone(),
        )));
        registry.register(Arc::new(crate::tool::memory::RememberTool::new(
            memory.clone(),
        )));
        registry.register(Arc::new(crate::tool::memory::MemoryUpsertTool::new(
            memory.clone(),
        )));
        registry.register(Arc::new(crate::tool::memory::MemoryImportTool::new(
            memory.clone(),
        )));
        registry.register(Arc::new(crate::tool::memory::MemoryExportTool::new(
            memory.clone(),
        )));
    }

    if tools_enabled && config.tools_enabled && crate::tool::personal::enabled() {
        use crate::tool::personal::{AgendaTool, InboxCaptureTool, InboxListTool, MailReadTool};
        registry.register(Arc::new(InboxListTool::new(config.state_dir.clone())));
        registry.register(Arc::new(InboxCaptureTool::new(config.state_dir.clone())));
        registry.register(Arc::new(AgendaTool::new(config.state_dir.clone())));
        registry.register(Arc::new(MailReadTool));
    }

    // The MCP servers are connected once, here, and their tools joined to the
    // rest. A server that cannot be reached is skipped: it should not stop
    // the assistant from working.
    let mut mcp_instructions: Vec<String> = Vec::new();
    let servers = mcp_servers(config);
    if tools_enabled && config.tools_enabled && config.mcp_enabled && !servers.is_empty() {
        let registry_mcp = crate::mcp::McpRegistry::connect(&servers).await;
        if !registry_mcp.is_empty() {
            for tool in registry_mcp.tools().await {
                registry.register(tool);
            }
            mcp_instructions = registry_mcp.instructions().await;
        }
    }

    let system_prompt = with_skill_catalogue(&config.effective_system_prompt(), &skills);
    let system_prompt = with_memory(&system_prompt, &memory);
    let system_prompt = with_mcp(&system_prompt, &mcp_instructions);

    let agent_config = AgentConfig {
        max_iterations: config.max_iterations,
        tools_enabled: tools_enabled && config.tools_enabled,
        system_prompt: Some(system_prompt),
        approval: config.approval.clone(),
        ..Default::default()
    };

    Ok(Arc::new(Agent::new(
        provider,
        Arc::new(registry),
        agent_config,
    )))
}

/// Append the MCP servers' own instructions to a system prompt.
fn with_mcp(prompt: &str, instructions: &[String]) -> String {
    if instructions.is_empty() {
        return prompt.to_string();
    }
    let mut out = String::from(prompt);
    out.push_str("\n\n<mcp_servers>\n");
    for instruction in instructions {
        out.push_str(instruction);
        out.push('\n');
    }
    out.push_str("</mcp_servers>");
    out
}

/// Append the skill catalogue to a system prompt.
fn with_skill_catalogue(prompt: &str, skills: &crate::skill::SkillSet) -> String {
    let catalogue = skills.catalog_prompt();
    if catalogue.is_empty() {
        return prompt.to_string();
    }
    format!(
        "{prompt}\n\n{catalogue}\n\n\
         When one of these skills fits the task, call the `skill` tool with its \
         name to read the instructions before you start."
    )
}

/// Append the standing memory to a system prompt.
fn with_memory(prompt: &str, memory: &crate::memory::MemoryStore) -> String {
    let section = memory.prompt_section();
    if section.is_empty() {
        return prompt.to_string();
    }
    format!("{prompt}\n\n{section}")
}

/// Run a single prompt and print the answer.
async fn one_shot(
    config: Config,
    text: String,
    no_tools: bool,
    is_pipe: bool,
    images: Vec<PathBuf>,
) -> Result<()> {
    // The pictures are read before the agent is built, so a file that cannot
    // be read is refused before anything is asked.
    let pictures = load_images(&images)?;
    let agent = build_agent(&config, !no_tools, None, None).await?;
    let sessions = Arc::new(tokio::sync::Mutex::new(SessionStore::open(
        config.state_dir.join("sessions"),
    )?));
    let runner = SessionRunner::new(agent.clone(), sessions);

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<crate::agent::AgentEvent>(256);
    let session_id = if is_pipe {
        "pipe".to_string()
    } else {
        "cli".to_string()
    };

    // Nobody can answer a permission request in a one-shot run, so the
    // approver reports the session as non-interactive and the
    // `on_no_interaction` policy decides.
    let approver: Arc<dyn crate::agent::ApprovalHandler> = Arc::new(NoInteraction);
    runner.set_approver(&session_id, approver).await;

    let message = if pictures.is_empty() {
        crate::provider::types::Message::user(text)
    } else {
        crate::provider::types::Message::user_with_images(text, pictures)
    };

    let runner_clone = runner.clone();
    let id = session_id.clone();
    let handle = tokio::spawn(async move {
        runner_clone.run_message(&id, Some(message), event_tx).await;
    });

    // Render the turn to stdout.
    let mut printed_anything = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            crate::agent::AgentEvent::TextDelta(text) => {
                print!("{text}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                printed_anything = true;
            }
            crate::agent::AgentEvent::ToolCallStarted { name, .. } => {
                eprintln!("· {name}");
            }
            crate::agent::AgentEvent::Error(message) => {
                eprintln!("error: {message}");
            }
            crate::agent::AgentEvent::Notice(message) => {
                eprintln!("note: {message}");
            }
            _ => {}
        }
    }
    let _ = handle.await;
    if printed_anything {
        println!();
    }

    Ok(())
}

/// Keep the curated memory in order, now and then.
///
/// The task runs on its own, so the assistant is not held up by it. A
/// failure is only worth a warning: the assistant works without it.
fn spawn_maintenance(config: &Config) {
    if !config.memory.consolidate {
        return;
    }

    let Ok(provider) = config.build_provider() else {
        return;
    };
    let Ok(memory) =
        crate::memory::MemoryStore::open(config.state_dir.join("memory"), config.workspace.clone())
    else {
        return;
    };

    let hours = config.memory.every_hours.clamp(1, 24);
    crate::memory::consolidate::spawn(
        Arc::new(memory),
        provider,
        std::time::Duration::from_secs(hours * 3600),
    );
}

/// Read the pictures that were named on the command line.
fn load_images(paths: &[PathBuf]) -> Result<Vec<crate::provider::types::ContentBlock>> {
    let mut blocks = Vec::new();
    for path in paths {
        let image = crate::image::Image::from_file(path).map_err(|e| anyhow::anyhow!("{e}"))?;
        blocks.push(crate::provider::types::ContentBlock::Image {
            media_type: image.media_type,
            data: image.data,
        });
    }
    Ok(blocks)
}

/// Read all of stdin as a single string.
async fn read_stdin() -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut buffer = String::new();
    tokio::io::stdin().read_to_string(&mut buffer).await?;
    Ok(buffer)
}

/// How often `lia serve` checks for routines that are due.
const ROUTINE_TICK: std::time::Duration = std::time::Duration::from_secs(30);
/// The longest one check may take, so a stuck channel cannot stall later ones.
const ROUTINE_TICK_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

fn routine_status_line(status: &crate::routine::RoutineStatus) -> String {
    use crate::routine::RunOutcome;
    let outcome = match &status.last_outcome {
        None => "never run".to_string(),
        Some(RunOutcome::Delivered) => "delivered".into(),
        Some(RunOutcome::Retrying { error }) => format!("retrying ({error})"),
        Some(RunOutcome::Failed { error }) => format!("failed ({error})"),
        Some(RunOutcome::Missed) => "missed".into(),
    };
    let last = status
        .last_run
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "-".into());
    let next = status
        .next_run
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "-".into());
    format!(
        "{} [{}] {} ({}) -> {}:{} | last: {} at {} | next: {}",
        status.id,
        if status.enabled { "enabled" } else { "paused" },
        status.schedule,
        status.timezone,
        status.target.channel,
        status.target.conversation,
        outcome,
        last,
        next,
    )
}

pub fn run_routine(
    state_dir: &Path,
    command: RoutineCommand,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    use crate::routine::{NewRoutine, RoutineStore, RoutineTarget};

    let mut store = RoutineStore::open(state_dir.join("routines"))?;
    match command {
        RoutineCommand::Create {
            id,
            when,
            timezone,
            channel,
            conversation,
            due_items,
            upcoming_jobs,
            approved_notes,
            max_bytes,
        } => {
            let when = crate::schedule::parse_when(&when, now).map_err(anyhow::Error::msg)?;
            let schedule =
                crate::schedule::ZonedSchedule::new(when, &timezone).map_err(anyhow::Error::msg)?;
            let status = store
                .create(
                    NewRoutine {
                        id,
                        owner: crate::inbox_identity::LOCAL_OWNER_ID.to_string(),
                        schedule,
                        sources: crate::briefing::BriefingSources {
                            due_items,
                            upcoming_jobs,
                            approved_notes,
                        },
                        max_bytes,
                        target: RoutineTarget {
                            channel: channel.trim().to_string(),
                            conversation: conversation.trim().to_string(),
                        },
                    },
                    now,
                )
                .map_err(anyhow::Error::msg)?;
            println!("created {}", routine_status_line(&status));
        }
        RoutineCommand::List => {
            let statuses = store.statuses();
            if statuses.is_empty() {
                println!("no routines");
            }
            for status in &statuses {
                println!("{}", routine_status_line(status));
            }
        }
        RoutineCommand::Pause { id } => {
            let status = store.pause(&id).map_err(anyhow::Error::msg)?;
            println!("paused {}", routine_status_line(&status));
        }
        RoutineCommand::Resume { id } => {
            let status = store.resume(&id, now).map_err(anyhow::Error::msg)?;
            println!("resumed {}", routine_status_line(&status));
        }
        RoutineCommand::Remove { id } => {
            if !store.remove(&id)? {
                anyhow::bail!("no routine \"{id}\"");
            }
            println!("removed {id}");
        }
    }
    Ok(())
}

/// Run what is due from the persisted routines. The stores are reloaded each
/// time so changes made by `lia routine` take effect without a restart.
pub async fn tick_routines(
    state_dir: &Path,
    scheduler: &crate::schedule::Scheduler,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<crate::routine::RoutineStatus> {
    let mut routines = match crate::routine::RoutineStore::open(state_dir.join("routines")) {
        Ok(routines) => routines,
        Err(e) => {
            tracing::warn!("could not read the routines: {e}");
            return Vec::new();
        }
    };
    let inbox = crate::inbox::InboxStore::open(state_dir.join("inbox")).ok();
    match tokio::time::timeout(
        ROUTINE_TICK_LIMIT,
        routines.tick_with_scheduler(now, inbox.as_ref(), scheduler),
    )
    .await
    {
        Ok(ran) => ran,
        Err(_) => {
            tracing::warn!("a routine check took too long and was stopped");
            Vec::new()
        }
    }
}

/// Push due reminders to the chat named by `LIA_REMINDER_CHANNEL` and
/// `LIA_REMINDER_CONVERSATION`. Without both, nothing is pushed.
async fn push_due_reminders(
    state_dir: &Path,
    scheduler: &crate::schedule::Scheduler,
    now: chrono::DateTime<chrono::Utc>,
) {
    let (Some(channel), Some(conversation)) = (
        crate::secrets::var("LIA_REMINDER_CHANNEL"),
        crate::secrets::var("LIA_REMINDER_CONVERSATION"),
    ) else {
        return;
    };
    let target = crate::routine::RoutineTarget {
        channel,
        conversation,
    };
    let Ok(owner) = crate::inbox::Owner::new(crate::inbox_identity::LOCAL_OWNER_ID) else {
        return;
    };
    let Ok(mut store) = crate::inbox::InboxStore::open(state_dir.join("inbox")) else {
        return;
    };
    let lookup = |name: &str| scheduler.channel(name);
    let pass = crate::reminder::deliver_due(&mut store, &owner, now, &target, &lookup);
    if tokio::time::timeout(ROUTINE_TICK_LIMIT, pass)
        .await
        .is_err()
    {
        tracing::warn!("a reminder check took too long and was stopped");
    }
}

fn spawn_routine_loop(
    state_dir: PathBuf,
    scheduler: Arc<crate::schedule::Scheduler>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(ROUTINE_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let now = chrono::Utc::now();
            for status in tick_routines(&state_dir, &scheduler, now).await {
                tracing::info!(routine = %status.id, "routine ran");
            }
            push_due_reminders(&state_dir, &scheduler, now).await;
        }
    })
}

/// Start the gateway, optionally with the Telegram channel attached.
async fn serve_gateway(
    config: Config,
    host: Option<String>,
    port: Option<u16>,
    token: Option<String>,
    with_channels: bool,
) -> Result<()> {
    let mut config = config;
    if let Some(host) = host {
        config.gateway.host = host;
    }
    if let Some(port) = port {
        config.gateway.port = port;
    }
    if let Some(token) = token {
        config.gateway.token = Some(token);
    }

    // The scheduler is built after the agent, but the agent's tools need a
    // place to find it, so they are given a slot to look in. The same is
    // true of the channels: a message the assistant starts on its own is
    // addressed through a hub that is filled when the channels start.
    let slot = crate::tool::schedule::SchedulerSlot::new();
    let hub = crate::channel::ChannelHub::new();
    let agent = build_agent(
        &config,
        config.tools_enabled,
        Some(slot.clone()),
        Some(hub.clone()),
    )
    .await?;
    let sessions = Arc::new(tokio::sync::Mutex::new(SessionStore::open(
        config.state_dir.join("sessions"),
    )?));

    let gateway = crate::gateway::Gateway::start(agent, sessions, Arc::new(config.clone())).await?;
    println!("gateway listening on {}", gateway.addr());
    println!("the page is at http://{}/", gateway.addr());
    if let Some(token) = &config.gateway.token {
        println!("authentication: bearer token required");
        let _ = token;
    }

    // The work the assistant promises to do later. The jobs are kept in the
    // state directory, so they survive a restart.
    let runner = SessionRunner::new(
        gateway.state().agent.clone(),
        gateway.state().sessions.clone(),
    );
    let approvers = crate::channel::Approvers::new();
    let store = crate::schedule::JobStore::open(config.state_dir.join("schedule"))?;
    let scheduler = Arc::new(crate::schedule::Scheduler::with_approvers(
        store,
        runner.clone(),
        approvers.clone(),
    ));
    slot.set(scheduler.clone());
    let _scheduler = scheduler.clone().spawn();

    if with_channels {
        // Every channel the environment configures is wired to the agent.
        // People who ask to be let in are kept in the state directory, so
        // an approval survives a restart.
        let pairing = Arc::new(crate::channel::PairingStore::open(
            config.state_dir.join("pairing.json"),
        )?);
        let channels = crate::channel::channels_from_env(Some(pairing), &config.state_dir);
        if channels.is_empty() {
            println!(
                "no channels enabled (run `lia setup`, or set TELEGRAM_BOT_TOKEN, DISCORD_BOT_TOKEN, SLACK_BOT_TOKEN; for WhatsApp just start `lia serve` and scan the qr code)"
            );
        } else {
            let (event_tx, _event_rx) = tokio::sync::mpsc::channel(256);

            for channel in &channels {
                println!("{} channel enabled", channel.name());
                // A job that answers in this channel is sent through it.
                scheduler.add_channel(channel.clone());
                // A message the assistant starts on her own is sent through
                // a channel looked up by name.
                hub.add(channel.clone());
            }

            let _handles = crate::channel::spawn_channels(channels, runner, event_tx, approvers);
        }
    }

    // Started after the channels are registered, so a delivery can find them.
    let routines = spawn_routine_loop(config.state_dir.clone(), scheduler.clone());

    // Wait for the gateway to shut down.
    let _ = tokio::signal::ctrl_c().await;
    println!("shutting down");
    routines.abort();
    gateway.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod inbox_cli_tests {
    use super::*;

    #[test]
    fn inbox_commands_share_local_owner_and_complete_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();

        run_inbox(
            state_dir,
            InboxCommand::Capture {
                text: "  call the dentist  ".into(),
                kind: InboxKind::Task,
                priority: InboxPriority::High,
                due_at: None,
            },
        )
        .unwrap();

        let owner = crate::inbox::Owner::new(crate::inbox_identity::LOCAL_OWNER_ID).unwrap();
        let store = crate::inbox::InboxStore::open(state_dir.join("inbox")).unwrap();
        let record = store
            .list(&owner, &crate::inbox::Filter::default())
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(record.text, "call the dentist");
        assert_eq!(record.kind, crate::inbox::Kind::Task);
        assert_eq!(record.priority, crate::inbox::Priority::High);
        drop(store);

        run_inbox(
            state_dir,
            InboxCommand::Defer {
                id: record.id,
                until: "2030-04-05T06:07:08Z".into(),
            },
        )
        .unwrap();
        run_inbox(state_dir, InboxCommand::Complete { id: record.id }).unwrap();
        run_inbox(state_dir, InboxCommand::Archive { id: record.id }).unwrap();

        let store = crate::inbox::InboxStore::open(state_dir.join("inbox")).unwrap();
        let archived = store.get(&owner, record.id).unwrap();
        assert_eq!(archived.state, crate::inbox::State::Archived);
        assert_eq!(
            archived.snoozed_until.unwrap().to_rfc3339(),
            "2030-04-05T06:07:08+00:00"
        );
        drop(store);

        run_inbox(state_dir, InboxCommand::Delete { id: record.id }).unwrap();
        assert!(
            crate::inbox::InboxStore::open(state_dir.join("inbox"))
                .unwrap()
                .get(&owner, record.id)
                .is_err()
        );
    }

    #[test]
    fn inbox_time_requires_rfc3339() {
        assert!(parse_inbox_time("tomorrow").is_err());
    }
}
