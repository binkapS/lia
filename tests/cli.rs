//! Tests of the command line itself: what the commands are called and what
//! they take.

use clap::Parser;
use lia::cli::{Cli, Command, InboxCommand, MailCommand, RoutineCommand};

mod common;

/// The command that was given.
fn command_of(args: &[&str]) -> Command {
    let mut full = vec!["lia"];
    full.extend_from_slice(args);
    Cli::parse_from(full).command.expect("a command")
}

#[test]
fn a_prompt_may_carry_a_picture() {
    let Command::Prompt { text, image, .. } =
        command_of(&["prompt", "what is this?", "--image", "a.png"])
    else {
        panic!("not a prompt");
    };
    assert_eq!(text, "what is this?");
    assert_eq!(image.len(), 1);
    assert_eq!(image[0].to_string_lossy(), "a.png");

    // More than one picture may be named.
    let Command::Prompt { image, .. } = command_of(&[
        "prompt",
        "these two",
        "--image",
        "a.png",
        "--image",
        "b.png",
    ]) else {
        panic!("not a prompt");
    };
    assert_eq!(image.len(), 2);
}

#[test]
fn the_memory_command_says_how_many_days_to_show() {
    let Command::Memory { days } = command_of(&["memory"]) else {
        panic!("not the memory command");
    };
    assert_eq!(days, 3, "the default is three days");

    let Command::Memory { days } = command_of(&["memory", "--days", "7"]) else {
        panic!("not the memory command");
    };
    assert_eq!(days, 7);
}

#[test]
fn the_agenda_command_takes_a_timezone_defaulting_to_utc() {
    let Command::Agenda { timezone } = command_of(&["agenda"]) else {
        panic!("not the agenda command");
    };
    assert_eq!(timezone, "UTC");
    let Command::Agenda { timezone } = command_of(&["agenda", "--timezone", "Europe/Paris"]) else {
        panic!("not the agenda command");
    };
    assert_eq!(timezone, "Europe/Paris");
}

#[test]
fn mail_commands_need_explicit_message_ids() {
    let Command::Mail {
        command: MailCommand::Read { ids },
    } = command_of(&["mail", "read", "m1", "m2"])
    else {
        panic!("not mail read");
    };
    assert_eq!(ids, ["m1", "m2"]);
    assert!(Cli::try_parse_from(["lia", "mail", "read"]).is_err());
}

#[test]
fn the_doctor_command_takes_nothing() {
    assert!(matches!(command_of(&["doctor"]), Command::Doctor));
}

#[test]
fn the_setup_command_can_show_and_store() {
    let Command::Setup { show, set, unset } = command_of(&["setup"]) else {
        panic!("not the setup command");
    };
    assert!(!show);
    assert!(set.is_empty());
    assert!(unset.is_empty());

    let Command::Setup { show, set, unset } = command_of(&[
        "setup",
        "--show",
        "--set",
        "OPENAI_API_KEY=sk-test",
        "--unset",
        "TELEGRAM_BOT_TOKEN",
    ]) else {
        panic!("not the setup command");
    };
    assert!(show);
    assert_eq!(set, vec!["OPENAI_API_KEY=sk-test".to_string()]);
    assert_eq!(unset, vec!["TELEGRAM_BOT_TOKEN".to_string()]);
}

#[test]
fn the_pair_command_takes_an_action_and_a_code() {
    let Command::Pair { action, code } = command_of(&["pair"]) else {
        panic!("not the pair command");
    };
    assert_eq!(action, "list");
    assert!(code.is_none());

    let Command::Pair { action, code } = command_of(&["pair", "approve", "ABCD-2345"]) else {
        panic!("not the pair command");
    };
    assert_eq!(action, "approve");
    assert_eq!(code.as_deref(), Some("ABCD-2345"));
}

#[test]
fn inbox_commands_parse_capture_filters_and_record_actions() {
    let Command::Inbox {
        command:
            InboxCommand::Capture {
                text,
                kind,
                priority,
                due_at,
            },
    } = command_of(&[
        "inbox",
        "capture",
        "buy milk",
        "--kind",
        "task",
        "--priority",
        "high",
        "--due-at",
        "2030-04-05T06:07:08Z",
    ])
    else {
        panic!("not an inbox capture");
    };
    assert_eq!(text, "buy milk");
    assert_eq!(kind, lia::cli::InboxKind::Task);
    assert_eq!(priority, lia::cli::InboxPriority::High);
    assert_eq!(due_at.as_deref(), Some("2030-04-05T06:07:08Z"));

    assert!(matches!(
        command_of(&["inbox", "list", "--state", "open", "--include-archived"]),
        Command::Inbox {
            command: InboxCommand::List {
                state: Some(lia::cli::InboxState::Open),
                include_archived: true,
                ..
            }
        }
    ));

    let id = uuid::Uuid::new_v4().to_string();
    assert!(matches!(
        command_of(&["inbox", "defer", &id, "--until", "2030-04-05T06:07:08Z"]),
        Command::Inbox {
            command: InboxCommand::Defer { until, .. }
        } if until == "2030-04-05T06:07:08Z"
    ));
    assert!(matches!(
        command_of(&["inbox", "complete", &id]),
        Command::Inbox {
            command: InboxCommand::Complete { .. }
        }
    ));
    assert!(matches!(
        command_of(&["inbox", "show", &id]),
        Command::Inbox {
            command: InboxCommand::Show { .. }
        }
    ));
    assert!(matches!(
        command_of(&["inbox", "archive", &id]),
        Command::Inbox {
            command: InboxCommand::Archive { .. }
        }
    ));
    assert!(matches!(
        command_of(&["inbox", "delete", &id]),
        Command::Inbox {
            command: InboxCommand::Delete { .. }
        }
    ));
}

fn create_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "routine",
        "create",
        "morning",
        "--when",
        "every day at 9:00",
        "--timezone",
        "UTC",
        "--channel",
        "fake",
        "--conversation",
        "room",
    ];
    args.extend_from_slice(extra);
    args
}

#[test]
fn a_routine_names_its_time_zone_target_and_sources() {
    let Command::Routine {
        command:
            RoutineCommand::Create {
                id,
                timezone,
                channel,
                conversation,
                due_items,
                upcoming_jobs,
                approved_notes,
                max_bytes,
                ..
            },
    } = command_of(&create_args(&["--due-items", "--approved-notes"]))
    else {
        panic!("not a routine create");
    };
    assert_eq!(
        (
            id.as_str(),
            timezone.as_str(),
            channel.as_str(),
            conversation.as_str()
        ),
        ("morning", "UTC", "fake", "room")
    );
    assert!(
        due_items && approved_notes && !upcoming_jobs,
        "sources are opt-in"
    );
    assert_eq!(max_bytes, 8192);

    for missing in ["--timezone", "--channel", "--conversation", "--when"] {
        let mut args = vec!["lia"];
        args.extend(create_args(&[]));
        let at = args.iter().position(|a| *a == missing).unwrap();
        args.drain(at..=at + 1);
        assert!(Cli::try_parse_from(args).is_err(), "{missing} is required");
    }
    assert!(matches!(
        command_of(&["routine", "pause", "morning"]),
        Command::Routine {
            command: RoutineCommand::Pause { .. }
        }
    ));
}

#[test]
fn routines_need_a_source_and_a_valid_time_zone() {
    let dir = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now();
    let create = |extra: &[&str], tz: &str| {
        let mut args = create_args(extra);
        let at = args.iter().position(|a| *a == "UTC").unwrap();
        args[at] = tz;
        let Command::Routine { command } = command_of(&args) else {
            panic!("not a routine");
        };
        lia::cli::run_routine(dir.path(), command, now)
    };
    assert!(create(&[], "UTC").is_err(), "no source selected");
    assert!(create(&["--due-items"], "Nowhere/Land").is_err());
    assert!(create(&["--due-items"], "UTC").is_ok());
    assert!(create(&["--due-items"], "UTC").is_err(), "duplicate id");
}

#[tokio::test]
async fn serving_delivers_a_due_routine_through_a_registered_channel() {
    use chrono::TimeZone;
    use lia::agent::session::SessionStore;
    use lia::agent::{Agent, AgentConfig, SessionRunner};
    use lia::routine::{RoutineStore, RunOutcome};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let created = chrono::Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
    let Command::Routine { command } = command_of(&create_args(&["--due-items"])) else {
        panic!("not a routine");
    };
    lia::cli::run_routine(dir.path(), command, created).unwrap();

    let agent = Agent::new(
        Arc::new(common::MockProvider::answering("unused")),
        Arc::new(lia::tool::ToolRegistry::new()),
        AgentConfig::default(),
    );
    let runner = SessionRunner::new(
        Arc::new(agent),
        Arc::new(tokio::sync::Mutex::new(SessionStore::in_memory())),
    );
    let scheduler = lia::schedule::Scheduler::in_memory(runner);
    let channel = Arc::new(common::FakeChannel::new());

    // Not due yet, and nothing is sent before the channel is registered.
    let early = created + chrono::Duration::hours(1);
    assert!(
        lia::cli::tick_routines(dir.path(), &scheduler, early)
            .await
            .is_empty()
    );

    let due = created + chrono::Duration::hours(9) + chrono::Duration::minutes(1);
    let ran = lia::cli::tick_routines(dir.path(), &scheduler, due).await;
    assert!(matches!(
        ran[0].last_outcome,
        Some(RunOutcome::Retrying { .. })
    ));

    scheduler.add_channel(channel.clone());
    let retry = due + chrono::Duration::minutes(6);
    let ran = lia::cli::tick_routines(dir.path(), &scheduler, retry).await;
    assert_eq!(ran[0].last_outcome, Some(RunOutcome::Delivered));
    let sent = channel.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].conversation, "room");

    let store = RoutineStore::open(dir.path().join("routines")).unwrap();
    assert_eq!(
        store.get("morning").unwrap().last_outcome,
        Some(RunOutcome::Delivered)
    );
}
