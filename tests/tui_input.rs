//! The message box: the buffer, the key map, and the wrapping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lia::tui::app::Line;
use lia::tui::{App, handle_key, render, wrap};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// A fresh interface.
fn app() -> App {
    App::new(lia::Config::default())
}

/// Draw the interface, so the app learns how tall the transcript is.
fn draw(app: &mut App) {
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    render::draw(&mut terminal, app).unwrap();
}

/// Press a key.
async fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    handle_key(app, KeyEvent::new(code, modifiers)).await;
}

/// Type a word.
async fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c), KeyModifiers::NONE).await;
    }
}

#[tokio::test]
async fn a_multibyte_character_is_deleted_whole() {
    let mut app = app();
    type_text(&mut app, "é").await;
    assert_eq!(app.input.text(), "é");

    press(&mut app, KeyCode::Backspace, KeyModifiers::NONE).await;
    assert_eq!(app.input.text(), "", "the character was not deleted whole");
}

#[tokio::test]
async fn a_character_is_put_in_at_the_cursor() {
    let mut app = app();
    type_text(&mut app, "ab").await;
    press(&mut app, KeyCode::Left, KeyModifiers::NONE).await;
    type_text(&mut app, "c").await;
    assert_eq!(app.input.text(), "acb");
}

#[tokio::test]
async fn the_delete_key_removes_the_character_under_the_cursor() {
    let mut app = app();
    type_text(&mut app, "ab").await;
    press(&mut app, KeyCode::Left, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Delete, KeyModifiers::NONE).await;
    assert_eq!(app.input.text(), "a");
}

#[tokio::test]
async fn ctrl_j_makes_a_new_line() {
    let mut app = app();
    type_text(&mut app, "a").await;
    press(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL).await;
    type_text(&mut app, "b").await;
    assert_eq!(app.input.text(), "a\nb");
}

#[tokio::test]
async fn alt_enter_makes_a_new_line() {
    let mut app = app();
    type_text(&mut app, "a").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::ALT).await;
    type_text(&mut app, "b").await;
    assert_eq!(app.input.text(), "a\nb");
}

#[tokio::test]
async fn a_paste_is_put_in_whole() {
    let mut app = app();
    app.input.insert_str("one\ntwo\nthree");
    assert_eq!(app.input.text(), "one\ntwo\nthree");
    assert_eq!(app.input.cursor(), 13);
}

#[tokio::test]
async fn up_moves_between_lines_and_then_reaches_the_history() {
    let mut app = app();
    app.input.insert_str("one\ntwo\nthree");
    app.input.set_cursor_at(13);

    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.input.cursor(), 7, "the cursor did not move up a line");

    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.input.cursor(), 3, "the cursor did not move up a line");

    // There is a message in the history to reach for.
    app.history.push("earlier".into());
    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.input.text(), "earlier", "the history was not reached");
}

#[tokio::test]
async fn the_draft_survives_a_history_recall() {
    let mut app = app();
    app.history.push("earlier".into());
    type_text(&mut app, "draft").await;

    press(&mut app, KeyCode::Up, KeyModifiers::NONE).await;
    assert_eq!(app.input.text(), "earlier");

    press(&mut app, KeyCode::Down, KeyModifiers::NONE).await;
    assert_eq!(app.input.text(), "draft", "the draft was lost");
}

#[tokio::test]
async fn ctrl_p_and_ctrl_n_reach_the_history() {
    let mut app = app();
    app.history.push("one".into());
    app.history.push("two".into());

    press(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL).await;
    assert_eq!(app.input.text(), "two");

    press(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL).await;
    assert_eq!(app.input.text(), "one");

    press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL).await;
    assert_eq!(app.input.text(), "two");
}

#[tokio::test]
async fn ctrl_u_kills_to_the_start_of_the_line() {
    let mut app = app();
    type_text(&mut app, "hello world").await;
    press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL).await;
    assert_eq!(app.input.text(), "");
}

#[tokio::test]
async fn ctrl_k_kills_to_the_end_of_the_line() {
    let mut app = app();
    type_text(&mut app, "hello world").await;
    press(&mut app, KeyCode::Home, KeyModifiers::NONE).await;
    press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL).await;
    assert_eq!(app.input.text(), "");
}

#[tokio::test]
async fn ctrl_w_kills_the_word() {
    let mut app = app();
    type_text(&mut app, "hello world").await;
    press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL).await;
    assert_eq!(app.input.text(), "hello ");
}

#[tokio::test]
async fn ctrl_left_moves_by_a_word() {
    let mut app = app();
    type_text(&mut app, "hello world").await;
    press(&mut app, KeyCode::Left, KeyModifiers::CONTROL).await;
    assert_eq!(app.input.cursor(), 6, "the cursor is not before the word");
}

#[tokio::test]
async fn home_and_end_work_on_the_line() {
    let mut app = app();
    app.input.insert_str("one\ntwo");
    app.input.set_cursor_at(5);

    press(&mut app, KeyCode::Home, KeyModifiers::NONE).await;
    assert_eq!(app.input.cursor(), 4, "home went to the start of the text");

    press(&mut app, KeyCode::Home, KeyModifiers::CONTROL).await;
    assert_eq!(app.input.cursor(), 0, "ctrl-home did not go to the start");
}

#[tokio::test]
async fn a_slash_command_is_remembered() {
    let mut app = app();
    type_text(&mut app, "/help").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.history, vec!["/help".to_string()]);
}

#[tokio::test]
async fn the_config_command_changes_and_saves_a_setting() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let mut app = app();
    app.config.config_path = Some(path.clone());

    type_text(&mut app, "/config set max_iterations 7").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;

    assert_eq!(app.config.max_iterations, 7);
    let saved = std::fs::read_to_string(&path).expect("the configuration was written");
    assert!(saved.contains("max_iterations = 7"), "{saved}");
}

#[tokio::test]
async fn the_config_command_refuses_a_bad_value() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let mut app = app();
    app.config.config_path = Some(path.clone());

    type_text(&mut app, "/config set temperature hot").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;

    assert!(!path.exists(), "a bad setting wrote the file");
    assert!(
        app.transcript
            .iter()
            .any(|line| matches!(line, Line::Error(text) if text.contains("temperature"))),
        "the bad value was not reported"
    );
}

#[tokio::test]
async fn the_input_is_empty_after_a_message_is_sent() {
    let mut app = app();
    type_text(&mut app, "hello").await;
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    assert_eq!(app.input.text(), "");
    assert_eq!(app.input.cursor(), 0);
}

#[tokio::test]
async fn the_page_keys_scroll_the_transcript() {
    let mut app = app();
    for index in 0..60 {
        app.transcript
            .push(Line::Assistant(format!("line {index}")));
    }
    draw(&mut app);
    let bottom = app.scroll;

    press(&mut app, KeyCode::PageUp, KeyModifiers::NONE).await;
    assert_eq!(
        app.scroll,
        bottom - app.page_rows(),
        "the page did not move the view"
    );

    press(&mut app, KeyCode::PageDown, KeyModifiers::NONE).await;
    assert_eq!(app.scroll, bottom, "the page did not come back");
}

#[tokio::test]
async fn alt_home_and_alt_end_go_to_the_ends_of_the_transcript() {
    let mut app = app();
    for index in 0..60 {
        app.transcript
            .push(Line::Assistant(format!("line {index}")));
    }
    draw(&mut app);

    press(&mut app, KeyCode::Home, KeyModifiers::ALT).await;
    assert_eq!(app.scroll, 0, "the view did not go to the oldest output");
    assert!(!app.follow, "the view still follows the newest output");

    press(&mut app, KeyCode::End, KeyModifiers::ALT).await;
    assert!(
        app.follow,
        "the view does not follow the newest output again"
    );
}

#[test]
fn a_long_line_is_wrapped() {
    let lines = wrap::wrap("one two three four five six", 10);
    assert!(lines.len() > 1, "the line was not wrapped");
    for line in &lines {
        assert!(
            line.text.chars().count() <= 10,
            "a row is too wide: {line:?}"
        );
    }
    // Every word is still there, in the same order, and no word is split
    // across two rows. A space at the edge of a row is dropped, because it
    // cannot be seen.
    let words: Vec<&str> = lines
        .iter()
        .flat_map(|l| l.text.split_whitespace())
        .collect();
    assert_eq!(words, vec!["one", "two", "three", "four", "five", "six"]);
}

#[test]
fn a_long_word_is_broken() {
    let lines = wrap::wrap("abcdefghijklmnop", 5);
    assert!(lines.len() >= 3, "the word was not broken");
    for line in &lines {
        assert!(
            line.text.chars().count() <= 5,
            "a row is too wide: {line:?}"
        );
    }
}

#[test]
fn a_line_break_starts_a_new_row() {
    let lines = wrap::wrap("one\ntwo", 40);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "one");
    assert_eq!(lines[1].text, "two");
}

#[test]
fn a_text_ending_in_a_line_break_has_a_row_for_the_cursor() {
    let lines = wrap::wrap("one\n", 40);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].text, "");
}

#[test]
fn the_cursor_is_where_it_should_be_after_wrapping() {
    let text = "one two three four";
    let lines = wrap::wrap(text, 8);
    let spot = wrap::cursor_spot(&lines, text.chars().count());
    assert!(spot.line >= 1, "the cursor is not on the second row");

    // The cursor is at the end of the text, so it is at the end of the last
    // row.
    let last = lines.last().unwrap();
    assert_eq!(spot.column, last.text.chars().count());
}

#[test]
fn a_wide_character_takes_two_cells() {
    let lines = wrap::wrap("日", 40);
    let spot = wrap::cursor_spot(&lines, 1);
    assert_eq!(spot.column, 2, "a wide character took one cell");
}
