//! The terminal user interface.
//!
//! A full-screen interface with a scrolling transcript, live tool call
//! panels, an input box, and a status bar. Rendering runs on its own task so
//! the agent's events can be applied to the state without blocking on the
//! terminal.

pub mod app;
pub mod commands;
pub mod input;
pub mod render;
pub mod theme;
pub mod wrap;

pub use app::App;
use app::Popup;
pub use input::Input;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEvent, KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::Config;
use crate::tool::approval::ApprovalDecision;

/// Run the terminal interface.
pub async fn run(config: Config) -> Result<()> {
    let mut terminal = setup_terminal()?;

    let mut app = App::new(config);
    let result = app.run(&mut terminal).await;

    restore_terminal(&mut terminal)?;
    result
}

/// Put the terminal into raw mode with the alternate screen.
fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        // A paste arrives as one piece of text instead of a stream of key
        // presses, so a pasted paragraph does not send itself half-way
        // through.
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

/// Restore the terminal to its original state.
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Handle a key press, returning whether the app should exit.
pub async fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // ctrl-c always quits, even while a tool is asking for permission.
    if ctrl && key.code == KeyCode::Char('c') {
        return true;
    }

    // While a tool waits for permission, only the answer keys count. The
    // agent is blocked, so anything else would just be noise.
    if app.is_awaiting_approval() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.resolve_approval(ApprovalDecision::Approve)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.resolve_approval(ApprovalDecision::Deny)
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                app.resolve_approval(ApprovalDecision::ApproveAlways)
            }
            _ => {}
        }
        return false;
    }

    // The session list has its own keys, so a stray character does not end
    // up in the input box while it is open.
    if app.popup == Popup::Sessions {
        match key.code {
            KeyCode::Esc => app.dismiss_popup(),
            KeyCode::Up => app.move_session_selection(-1),
            KeyCode::Down => app.move_session_selection(1),
            KeyCode::Enter => app.switch_session().await,
            _ => {}
        }
        return false;
    }

    // While the command preview is open it owns the selection keys. Enter
    // runs the highlighted command, so "/he" plus Enter is /help rather
    // than an unknown command.
    let before = app.input.text().to_string();
    if !app.command_matches().is_empty() {
        match key.code {
            KeyCode::Up => {
                app.move_command_selection(-1);
                return false;
            }
            KeyCode::Down => {
                app.move_command_selection(1);
                return false;
            }
            KeyCode::Char('p') if ctrl => {
                app.move_command_selection(-1);
                return false;
            }
            KeyCode::Char('n') if ctrl => {
                app.move_command_selection(1);
                return false;
            }
            KeyCode::Tab => {
                app.complete_command();
                return false;
            }
            KeyCode::Enter if !shift && !alt => {
                app.run_selected_command();
                return false;
            }
            KeyCode::Esc => {
                app.dismiss_command_preview();
                return false;
            }
            _ => {}
        }
    }

    match key.code {
        // A line break, three ways. Ctrl+J is tested before the arm that
        // swallows control-modified characters, or it would type a "j".
        KeyCode::Char('j') if ctrl => app.input.insert_char('\n'),
        KeyCode::Enter if shift || alt => app.input.insert_char('\n'),

        KeyCode::Enter => app.submit(),
        KeyCode::Backspace if alt || ctrl => {
            app.input.kill_word_backward();
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => {
            app.input.delete();
        }
        KeyCode::Left if ctrl || alt => {
            app.input.move_word_left();
        }
        KeyCode::Right if ctrl || alt => {
            app.input.move_word_right();
        }
        KeyCode::Left => app.cursor_left(),
        KeyCode::Right => app.cursor_right(),
        KeyCode::Home if ctrl => app.input.move_to_buffer_start(),
        KeyCode::End if ctrl => app.input.move_to_buffer_end(),
        // The ends of the transcript. These come before the plain home
        // and end, which belong to the message box.
        KeyCode::Home if alt => app.scroll_to_top(),
        KeyCode::End if alt => app.scroll_to_bottom(),
        KeyCode::Home => app.cursor_home(),
        KeyCode::End => app.cursor_end(),
        // Up and down move through the rows of the message box. Only at the
        // edge do they reach for the history.
        KeyCode::Up if ctrl => app.history_prev(),
        KeyCode::Down if ctrl => app.history_next(),
        KeyCode::Up => {
            if !app.move_visual_line(-1) {
                app.history_prev();
            }
        }
        KeyCode::Down => {
            if !app.move_visual_line(1) {
                app.history_next();
            }
        }
        KeyCode::Char('p') if ctrl => app.history_prev(),
        KeyCode::Char('n') if ctrl => app.history_next(),
        KeyCode::Char('u') if ctrl => {
            app.input.kill_to_line_start();
        }
        KeyCode::Char('k') if ctrl => {
            app.input.kill_to_line_end();
        }
        KeyCode::Char('w') if ctrl => {
            app.input.kill_word_backward();
        }
        KeyCode::PageUp => {
            let rows = app.page_rows();
            app.scroll_up(rows);
        }
        KeyCode::PageDown => {
            let rows = app.page_rows();
            app.scroll_down(rows);
        }
        KeyCode::Tab => app.complete(),
        KeyCode::F(1) => app.toggle_help(),
        // The microphone: press once to open it, again to close it.
        KeyCode::F(2) => app.toggle_listening(),
        KeyCode::Esc => app.escape(),
        // A control-modified character is a shortcut, not text. Without
        // this, ctrl-j would type a "j" into the input box.
        KeyCode::Char(_) if ctrl => {}
        KeyCode::Char(c) => app.insert(c),
        _ => {}
    }
    // Any edit starts the preview over, with the first suggestion picked.
    if app.input.text() != before {
        app.reset_command_preview();
    }
    false
}

/// Handle a mouse event.
///
/// The wheel scrolls the transcript, three rows a notch. A popup that is
/// asking for an answer takes the wheel away, as it takes the keys.
pub fn handle_mouse(app: &mut App, kind: MouseEventKind) {
    if app.is_awaiting_approval() || app.popup == Popup::Sessions {
        return;
    }
    match kind {
        MouseEventKind::ScrollUp => app.scroll_up(3),
        MouseEventKind::ScrollDown => app.scroll_down(3),
        _ => {}
    }
}

/// Whether a key event should be ignored by the app.
pub fn is_ignored_key(_key: &Event) -> bool {
    false
}
