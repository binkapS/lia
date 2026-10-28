//! Tests of letting people in who have asked to be let in.

use std::sync::Arc;

use lia::channel::pairing::{PairingStore, pairing_message};
use lia::channel::telegram::{AllowedChat, TelegramChannel, TelegramChat};
use lia::channel::{Control, parse_control};

/// A private chat, which is where a stranger reaches the bot.
fn private_chat(id: i64) -> TelegramChat {
    TelegramChat {
        id,
        title: None,
        username: None,
        kind: Some("private".into()),
    }
}

/// A group chat, where everybody can see what is said.
fn group_chat(id: i64) -> TelegramChat {
    TelegramChat {
        id,
        title: Some("the team".into()),
        username: None,
        kind: Some("group".into()),
    }
}

/// A channel that lets the owner in, with a pairing store.
fn channel(store: &Arc<PairingStore>) -> TelegramChannel {
    TelegramChannel::new("token", vec![AllowedChat::Id(1)]).with_pairing(store.clone())
}

#[test]
fn a_stranger_is_not_let_in() {
    let store = Arc::new(PairingStore::in_memory());
    let telegram = channel(&store);

    assert!(!telegram.may_talk(&private_chat(42), "42"));
    assert!(
        telegram.may_talk(&private_chat(1), "1"),
        "the owner's own chat was refused"
    );
}

#[test]
fn a_stranger_who_is_paired_is_let_in() {
    let store = Arc::new(PairingStore::in_memory());
    let telegram = channel(&store);

    let (request, offered) = store.request("telegram", "42", "42", Some("Ann".into()));
    assert!(offered, "the code was not offered");

    assert!(
        !telegram.may_talk(&private_chat(42), "42"),
        "let in too early"
    );

    store.approve(&request.code).expect("approved");
    assert!(telegram.may_talk(&private_chat(42), "42"), "not let in");
}

#[test]
fn a_group_is_never_paired() {
    let store = Arc::new(PairingStore::in_memory());
    let telegram = channel(&store);

    let (request, _) = store.request("telegram", "-100", "-100", None);
    store.approve(&request.code).expect("approved");

    assert!(
        !telegram.may_talk(&group_chat(-100), "-100"),
        "a group was let in as a whole"
    );
}

#[test]
fn one_person_being_paired_does_not_let_another_in() {
    let store = Arc::new(PairingStore::in_memory());
    let telegram = channel(&store);

    let (request, _) = store.request("telegram", "42", "42", Some("Ann".into()));
    store.approve(&request.code).expect("approved");

    assert!(
        !telegram.may_talk(&private_chat(42), "43"),
        "another person"
    );
}

#[test]
fn a_pairing_ends_when_it_is_revoked() {
    let store = Arc::new(PairingStore::in_memory());
    let telegram = channel(&store);

    let (request, _) = store.request("telegram", "42", "42", None);
    store.approve(&request.code).expect("approved");
    assert!(telegram.may_talk(&private_chat(42), "42"));

    store.revoke(&request.code);
    assert!(!telegram.may_talk(&private_chat(42), "42"), "still let in");
}

#[test]
fn without_a_store_only_the_allow_list_may_talk() {
    let telegram = TelegramChannel::new("token", vec![AllowedChat::Id(1)]);

    assert!(!telegram.may_talk(&private_chat(42), "42"));
    assert!(telegram.may_talk(&private_chat(1), "1"));
}

#[test]
fn the_code_is_told_to_the_person_who_asked() {
    let message = pairing_message("ABCD-2345");
    assert!(message.contains("ABCD-2345"), "{message}");
    assert!(
        message.contains("approve"),
        "the person is not told what to do: {message}"
    );
}

#[test]
fn a_pairing_command_is_read() {
    let Some(Control::Pair { code }) = parse_control("/pair ABCD-2345") else {
        panic!("the command was not read");
    };
    assert_eq!(code.as_deref(), Some("ABCD-2345"));

    let Some(Control::Pair { code }) = parse_control("/pair") else {
        panic!("the command was not read");
    };
    assert!(code.is_none(), "a code was made up: {code:?}");
}

#[test]
fn the_store_keeps_the_code_across_a_restart() {
    let dir = std::env::temp_dir().join(format!("lia-pair-{}", uuid::Uuid::new_v4()));
    let path = dir.join("pairing.json");

    let store = PairingStore::open(path.clone()).expect("opened");
    let (request, _) = store.request("telegram", "42", "42", Some("Ann".into()));
    store.approve(&request.code).expect("approved");

    let reopened = Arc::new(PairingStore::open(path).expect("reopened"));
    let telegram = channel(&reopened);
    assert!(
        telegram.may_talk(&private_chat(42), "42"),
        "not let in again"
    );

    let _ = std::fs::remove_dir_all(dir);
}
