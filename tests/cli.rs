//! Tests of the command line itself: what the commands are called and what
//! they take.

use clap::Parser;
use lia::cli::{Cli, Command};

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
