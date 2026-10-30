//! The message box as it is drawn.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use lia::agent::AgentEvent;
use lia::tui::app::Line;
use lia::tui::{App, handle_mouse, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Draw the interface and return what is on the screen, and where the
/// cursor ended up.
fn screen(app: &mut App, width: u16, height: u16) -> (String, (u16, u16)) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    render::draw(&mut terminal, app).unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    let cursor = terminal.get_cursor_position().unwrap();
    (text, (cursor.x, cursor.y))
}

#[test]
fn the_message_box_grows_with_the_text() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_str("first line");

    let (short, _) = screen(&mut app, 60, 20);
    assert!(short.contains("first line"));

    app.input.insert_str("\nsecond line\nthird line");
    let (long, _) = screen(&mut app, 60, 20);
    assert!(
        long.contains("second line"),
        "the second line is not on the screen:\n{long}"
    );
    assert!(long.contains("third line"));
}

#[test]
fn the_cursor_sits_where_it_should() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_str("hello");

    let (_, cursor) = screen(&mut app, 60, 12);
    // The box starts at the third cell of the row, after the border and
    // the prompt.
    assert_eq!(cursor, (3 + 5, 9), "the cursor is in the wrong place");
}

#[test]
fn the_cursor_moves_down_a_line() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_str("one\ntwo");

    let (_, cursor) = screen(&mut app, 60, 12);
    assert_eq!(cursor.1, 9, "the cursor is not on the second row");
}

#[test]
fn a_wide_character_takes_two_cells() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_str("日");

    let (_, cursor) = screen(&mut app, 60, 12);
    assert_eq!(cursor.0, 3 + 2, "a wide character took one cell");
}

#[test]
fn a_long_line_is_wrapped_inside_the_box() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_str(&"word ".repeat(30));

    let (text, _) = screen(&mut app, 40, 20);
    // Every row of the box is at most the width of the terminal.
    for row in text.lines() {
        assert!(row.chars().count() <= 40, "a row runs off the edge:\n{row}");
    }
}

#[test]
fn the_recording_is_shown_in_the_box() {
    let mut app = App::new(lia::Config::default());
    app.voice_state = lia::tui::app::VoiceState::Listening {
        level: 0.5,
        elapsed: 1.5,
    };

    let (text, _) = screen(&mut app, 60, 12);
    assert!(text.contains("REC"), "the recording is not shown:\n{text}");
    assert!(text.contains("1.5"), "the clock is not shown:\n{text}");
}

#[test]
fn live_mode_is_named_in_the_box() {
    let mut app = App::new(lia::Config::default());
    app.live = true;
    app.live_share = lia::config::LiveShare::Screen;
    app.voice_state = lia::tui::app::VoiceState::Listening {
        level: 0.5,
        elapsed: 1.5,
    };

    let (text, _) = screen(&mut app, 60, 12);
    assert!(text.contains("LIVE"), "live mode is not shown:\n{text}");
    assert!(
        text.contains("screen"),
        "what is shared is not shown:\n{text}"
    );
}

#[test]
fn the_level_meter_is_shown_while_listening() {
    let mut app = App::new(lia::Config::default());
    app.voice_state = lia::tui::app::VoiceState::Listening {
        level: 0.5,
        elapsed: 1.5,
    };
    app.voice_levels = vec![0.2, 0.6, 0.9];

    let (text, _) = screen(&mut app, 60, 12);
    // The meter is drawn with the bars of the level.
    let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    assert!(
        text.chars().any(|c| bars.contains(&c)),
        "the meter is not shown:\n{text}"
    );
}

/// A transcript with a line for each number.
fn transcript(app: &mut App, count: usize) {
    for index in 0..count {
        app.transcript
            .push(Line::Assistant(format!("line {index}")));
    }
}

#[test]
fn the_wheel_scrolls_the_transcript() {
    let mut app = App::new(lia::Config::default());
    transcript(&mut app, 40);

    let (bottom, _) = screen(&mut app, 60, 20);
    assert!(
        bottom.contains("line 39"),
        "the newest line is not on the screen:\n{bottom}"
    );

    // Three notches of the wheel, three rows each.
    for _ in 0..3 {
        handle_mouse(&mut app, MouseEventKind::ScrollUp);
    }
    let (up, _) = screen(&mut app, 60, 20);
    assert!(!up.contains("line 39"), "the view did not move:\n{up}");
    assert!(
        up.contains("line 22"),
        "the view moved the wrong way:\n{up}"
    );
    assert!(
        up.contains("▲ 9 rows"),
        "the footer does not say how far back the view is:\n{up}"
    );
}

#[test]
fn the_wheel_stops_at_the_oldest_output() {
    let mut app = App::new(lia::Config::default());
    transcript(&mut app, 40);

    // Far more notches than there are rows to scroll.
    for _ in 0..100 {
        handle_mouse(&mut app, MouseEventKind::ScrollUp);
    }
    let (top, _) = screen(&mut app, 60, 20);
    assert!(
        top.contains("line 0"),
        "the oldest line is not on the screen:\n{top}"
    );
    assert!(
        top.contains("▲ 25 rows"),
        "the footer counts the wrong number of rows:\n{top}"
    );
}

#[tokio::test]
async fn the_new_words_do_not_drag_the_view_along() {
    let mut app = App::new(lia::Config::default());
    transcript(&mut app, 40);

    let _ = screen(&mut app, 60, 20);
    handle_mouse(&mut app, MouseEventKind::ScrollUp);
    let (before, _) = screen(&mut app, 60, 20);

    // The assistant says something while the person is reading back.
    app.apply_event(AgentEvent::TextDelta("hello".into())).await;
    let (after, _) = screen(&mut app, 60, 20);

    assert!(
        after.contains("line 22") && after.contains("line 36"),
        "the view moved:\n{after}"
    );
    assert!(
        before.contains("line 22") && before.contains("line 36"),
        "the view was not where the test expects it:\n{before}"
    );
}

#[test]
fn the_view_follows_the_newest_output_again_at_the_bottom() {
    let mut app = App::new(lia::Config::default());
    transcript(&mut app, 40);

    let _ = screen(&mut app, 60, 20);
    handle_mouse(&mut app, MouseEventKind::ScrollUp);
    let _ = screen(&mut app, 60, 20);
    assert!(!app.follow, "the view still follows the newest output");

    // Back to the bottom, and it follows again.
    handle_mouse(&mut app, MouseEventKind::ScrollDown);
    let _ = screen(&mut app, 60, 20);
    assert!(
        app.follow,
        "the view does not follow the newest output again"
    );
}

#[tokio::test]
async fn the_activity_rail_names_the_running_tool() {
    let mut app = App::new(lia::Config::default());
    app.apply_event(AgentEvent::Started {
        model: "test-model".into(),
    })
    .await;
    app.apply_event(AgentEvent::ToolCallStarted {
        id: "call-1".into(),
        name: "shell".into(),
        input: serde_json::json!({ "command": "ls" }),
    })
    .await;
    app.tick = 3;

    let (text, _) = screen(&mut app, 80, 24);
    assert!(
        text.contains("WORKING"),
        "the activity rail is not shown:\n{text}"
    );
    assert!(
        text.contains("running shell"),
        "the action is not named:\n{text}"
    );
}

#[tokio::test]
async fn a_slash_opens_a_filtered_command_preview() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_char('/');

    let (all, _) = screen(&mut app, 80, 24);
    assert!(all.contains("COMMANDS"), "the preview is not shown:\n{all}");
    assert!(all.contains("/help"), "the commands are not listed:\n{all}");
    assert!(all.contains("/image"));
    assert!(all.contains("COMMANDS · 16"));

    for c in "he".chars() {
        lia::tui::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        )
        .await;
    }
    let (filtered, _) = screen(&mut app, 80, 24);
    assert!(
        filtered.contains("/help"),
        "the match is missing:\n{filtered}"
    );
    assert!(
        !filtered.contains("/handsfree"),
        "a command that does not match is still shown:\n{filtered}"
    );

    lia::tui::handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)).await;
    assert_eq!(app.input.text(), "/help ");
}

#[tokio::test]
async fn the_command_preview_follows_a_long_selection() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_char('/');

    for _ in 0..15 {
        lia::tui::handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).await;
    }
    let (text, _) = screen(&mut app, 80, 24);
    assert!(
        text.contains("/handsfree"),
        "the selected command scrolled out of the window:\n{text}"
    );
    assert!(
        text.contains("/quit"),
        "the end of the list is not visible:\n{text}"
    );
}

#[tokio::test]
async fn enter_runs_the_highlighted_command() {
    let mut app = App::new(lia::Config::default());
    app.input.insert_str("/he");

    lia::tui::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;

    assert_eq!(app.popup, lia::tui::app::Popup::Help);
    assert!(app.input.is_empty());
}
