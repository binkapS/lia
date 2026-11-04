//! The words the voice says, as the terminal composes them.
//!
//! The transcript keeps the detail; the speech is composed. These tests
//! read what the composer answers with, line by line, the way the
//! interface feeds it.

use lia::voice::compose::SpeechComposer;

/// The courtesies the voice opens a reply with, without the honorific.
const OPENERS: [&str; 7] = [
    "I'll get right on that",
    "Certainly",
    "Of course",
    "Very good",
    "Very well",
    "Let me handle that",
    "Going right ahead",
];

/// A fresh composer.
fn composer() -> SpeechComposer {
    SpeechComposer::new()
}

/// Feed whole lines, as an answer that arrives line by line.
fn spoken(lines: &[&str]) -> Vec<String> {
    let mut speech = SpeechComposer::new();
    let mut said = Vec::new();
    for line in lines {
        said.extend(speech.line(line));
    }
    said.extend(speech.flush());
    said
}

/// The words one sentence of prose is composed into.
fn spoken_words(text: &str) -> String {
    composer().spoken_chunk(text)
}

/// One of the courtesies, with the honorific if it came.
fn opens_with_a_courtesy(words: &str) -> bool {
    OPENERS.iter().any(|courtesy| words.starts_with(courtesy))
}

#[test]
fn the_first_sentence_opens_with_a_courtesy() {
    let words = spoken_words("The GPU sits at 71°C.");
    assert!(
        opens_with_a_courtesy(&words),
        "the reply was not opened with a courtesy: {words}"
    );
    assert!(words.contains("71 degrees"), "the sign was read: {words}");
    assert!(words.ends_with('.'), "the sentence was left open: {words}");
}

#[test]
fn a_courtesy_comes_once_per_reply() {
    let mut speech = composer();
    let _ = speech.spoken_chunk("Here is what I found.");
    let second = speech.spoken_chunk("The build is green.");
    assert!(
        !opens_with_a_courtesy(&second),
        "the courtesy came around again: {second}"
    );
}

#[test]
fn list_rows_are_read_first_then_and_finally() {
    let said = spoken(&["- clean the index", "- drop the cache", "- rerun the suite"]);
    let joined = said.join(" ");
    assert!(joined.contains("First, clean the index."), "{joined}");
    assert!(joined.contains("Then, drop the cache."), "{joined}");
    assert!(joined.contains("And finally, rerun the suite."), "{joined}");
    assert!(!joined.contains('-'), "the list mark was read: {joined}");
}

#[test]
fn a_numbered_list_reads_the_same_way() {
    let said = spoken(&["1. check the disk", "2. check the memory"]);
    let joined = said.join(" ");
    assert!(joined.contains("First, check the disk."), "{joined}");
    assert!(joined.contains("Then, check the memory."), "{joined}");
}

#[test]
fn a_roster_may_open_the_whole_reply() {
    let mut speech = composer();
    speech.line("- clean the index");
    speech.line("- drop the cache");
    let said = speech.flush();
    assert!(said.len() >= 3, "{said:?}");
    assert!(
        opens_with_a_courtesy(said.first().unwrap()),
        "the roster was not bowed onto: {said:?}"
    );
    assert!(said[1].starts_with("First, "), "{said:?}");
}

#[test]
fn a_long_list_leaves_the_rest_on_screen() {
    let items: Vec<String> = (1..=8).map(|n| format!("- step {n}")).collect();
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let said = spoken(&refs);
    let joined = said.join(" ");
    assert!(joined.contains("First, step 1."), "{joined}");
    assert!(joined.contains("And finally, step 5."), "{joined}");
    assert!(
        !joined.contains("step 8"),
        "the list was read five deep at most: {joined}"
    );
    assert!(joined.contains("3 more"), "{joined}");
    assert!(joined.contains("on your screen"), "{joined}");
}

#[test]
fn code_is_pointed_at_and_not_recited() {
    let said = spoken(&[
        "```sh",
        "cargo build --release",
        "```",
        "That is the build.",
    ]);
    let joined = said.join(" ");
    assert!(joined.contains("laid the code out on screen"), "{joined}");
    assert!(!joined.contains("cargo"), "the code was recited: {joined}");
}

#[test]
fn a_wide_table_is_announced_once() {
    let said = spoken(&[
        "| CPU | MEM | DISK | NET |",
        "| 42 | 61 | 71 | low |",
        "That is everything.",
    ]);
    let joined = said.join(" ");
    assert!(
        joined.matches("on the console").count() == 1,
        "the table was named twice or never: {joined}"
    );
    assert!(!joined.contains("42"), "the table was recited: {joined}");
}

#[test]
fn a_narrow_table_is_read_when_there_is_room() {
    let said = spoken(&["| CPU | MEM |"]);
    let joined = said.join(" ");
    assert!(joined.contains("CPU"), "the row was not read: {joined}");
}

#[test]
fn units_say_their_words() {
    let words = spoken_words("CPU 42%, north 12 MB/s, wake 120ms, and 2 GB free.");
    assert!(words.contains("42 percent"), "{words}");
    assert!(words.contains("12 megabytes per second"), "{words}");
    assert!(words.contains("120 milliseconds"), "{words}");
    assert!(words.contains("2 gigabytes"), "{words}");
}

#[test]
fn comparisons_say_what_they_mean() {
    let words = spoken_words("memory is at ≥ 80 GB free.");
    assert!(words.contains("at least 80 gigabytes"), "{words}");
    assert!(!words.contains("at at"), "the repeat was read: {words}");
}

#[test]
fn a_bare_link_is_pointed_at() {
    let said = spoken(&["The docs live at https://example.com/wiki/mm you."]);
    let joined = said.join(" ");
    assert!(joined.contains("the link on your screen"), "{joined}");
    assert!(
        !joined.contains("https"),
        "the address was spelled: {joined}"
    );
}

#[test]
fn the_say_command_says_the_words_as_they_are() {
    let speech = composer();
    assert_eq!(speech.direct("hello there"), "hello there");
    let with_code = speech.direct("```sh\ncargo build\n```\nthen please");
    assert_eq!(with_code, "code block then please");
}

#[test]
fn a_new_reply_leaves_the_last_one_behind() {
    let mut speech = composer();
    let _ = speech.line("```sh");
    speech.begin();
    let said = speech.line("A clean word.");
    assert!(
        matches!(said.last().map(String::as_str), Some(word) if word.contains("A clean word")),
        "the fence followed the composer into the next reply: {said:?}"
    );
}
