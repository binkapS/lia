//! Drawing the terminal interface.
//!
//! The layout is deliberately quiet, in the spirit of a coding agent's
//! terminal: one command bar, a wide reading column, a live action rail,
//! and a prompt at the bottom. On a wide terminal a narrow rail keeps the
//! session, model, plan, and token budget in view without interrupting the
//! conversation. Motion is reserved for work that is actually happening.

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RatLine, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::app::{App, Line, Popup};
use super::commands;
use super::theme::Theme;
use super::wrap;
use crate::tool::todo::TodoStatus;

/// The tallest the message box may get.
const MAX_INPUT_ROWS: u16 = 12;

/// The marks that turn while the assistant is working.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A wider, slower orbit for the activity rail.
const ORBIT: [&str; 8] = ["◐", "◓", "◑", "◒", "◐", "◓", "◑", "◒"];

/// The pulse of the voice meter.
const PULSE: [&str; 6] = ["▁", "▂", "▄", "▆", "▄", "▂"];

/// A terminal this wide gets the instrument rail on the left.
const RAIL_AT: u16 = 100;

/// How wide that rail is.
const RAIL_WIDTH: u16 = 27;

/// Make text safe and legible in the terminal.
///
/// Tabs and control characters have no meaningful width, and a terminal
/// renders them inconsistently — a tab can vanish entirely, or a stray escape
/// sequence can move the cursor somewhere unexpected. Both would corrupt the
/// layout, so tabs become spaces and other control characters are dropped.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Fit a piece of text into a number of cells, with an ellipsis if it does
/// not fit.
fn truncate(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + w > width - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Draw the whole interface.
pub fn draw<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<(), B::Error> {
    terminal.draw(|frame| {
        let area = frame.area();
        let theme = app.theme;

        // The whole interface sits on one ground.
        frame.render_widget(
            Block::default().style(Style::default().bg(theme.void)),
            area,
        );

        let rail = area.width >= RAIL_AT;
        // The plan is in the rail when there is one, and above the prompt
        // when there is not.
        let plan_height = if rail { 0 } else { plan_height(app) };
        let action_height = if app.busy { 1 } else { 0 };

        let mut constraints = vec![Constraint::Length(1), Constraint::Min(0)];
        if plan_height > 0 {
            constraints.push(Constraint::Length(plan_height));
        }
        if action_height > 0 {
            constraints.push(Constraint::Length(action_height));
        }
        constraints.push(Constraint::Length(input_height(app, area.width)));
        constraints.push(Constraint::Length(1));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        draw_header(frame, chunks[0], app);

        let body = chunks[1];
        if rail {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(RAIL_WIDTH), Constraint::Min(0)])
                .split(body);
            draw_rail(frame, columns[0], app);
            draw_transcript(frame, columns[1], app);
        } else {
            draw_transcript(frame, body, app);
        }

        let mut next = 2;
        if plan_height > 0 {
            draw_plan(frame, chunks[next], app);
            next += 1;
        }
        if action_height > 0 {
            draw_activity(frame, chunks[next], app);
            next += 1;
        }
        draw_input(frame, chunks[next], app);
        draw_footer(frame, chunks[next + 1], app);
        if app.popup == Popup::None {
            draw_command_preview(frame, chunks[next], app);
        }

        match &app.popup {
            Popup::Approval(pending) => draw_approval(frame, area, pending, app.theme),
            Popup::Sessions => draw_sessions(frame, area, app),
            Popup::Help => draw_help(frame, area, app.theme),
            Popup::None => {}
        }
    })?;
    Ok(())
}

/// How many lines the plan takes, if it is shown at all.
fn plan_height(app: &App) -> u16 {
    if app.plan.is_empty() {
        return 0;
    }
    // A border, a title, and the items that fit.
    let shown = app.plan.len().min(5) as u16;
    shown + 2
}

/// How many rows the message box takes, for a given width.
///
/// The box grows with the text, so a message of several lines can be read
/// and edited, and stops at a cap so the transcript keeps its room.
fn input_height(app: &App, width: u16) -> u16 {
    let inner = text_width(width);
    let rows = wrap::wrap(app.input.text(), inner).len().max(1) as u16;
    // The two borders.
    (rows + 2).clamp(3, MAX_INPUT_ROWS)
}

/// How wide the text of the message box is, in cells.
///
/// The box has a border on each side and a gutter of two cells at the
/// start of every row.
fn text_width(width: u16) -> usize {
    width.saturating_sub(4).max(1) as usize
}

/// How many cells a run of spans takes.
fn span_width(spans: &[Span]) -> usize {
    spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// A row with something on the left and something on the right.
fn split_line<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: u16) -> RatLine<'a> {
    let used = span_width(&left) + span_width(&right);
    let mut spans = left;
    if used < width as usize {
        spans.push(Span::raw(" ".repeat(width as usize - used)));
    }
    spans.extend(right);
    RatLine::from(spans)
}

/// The colour of the little pulse that says the machine is awake.
fn live_colour(theme: Theme, tick: u64) -> Color {
    match tick % 4 {
        0 => theme.accent,
        1 => theme.heading,
        2 => theme.info,
        _ => theme.accent,
    }
}

/// Draw the command bar: who is speaking, with what, and where.
fn draw_header(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let model = app.config.effective_model();
    let provider = app.config.provider.as_str();
    let narrow = area.width < 74;

    let mut left = vec![
        Span::styled("◆ ", Style::default().fg(theme.accent)),
        Span::styled(
            "LIA",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  /  AGENT CONSOLE", Style::default().fg(theme.muted)),
    ];
    if narrow {
        left.truncate(2);
    }

    let (mark, mark_colour) = if app.busy {
        (
            ORBIT[(app.tick as usize) % ORBIT.len()],
            live_colour(theme, app.tick),
        )
    } else {
        ("●", theme.ok)
    };

    let mut right = Vec::new();
    if !narrow {
        right.push(Span::styled(
            format!("{provider}/{model}"),
            Style::default().fg(theme.heading),
        ));
        right.push(Span::styled("   ", Style::default().fg(theme.muted)));
    }
    right.push(Span::styled(
        format!("{mark} "),
        Style::default().fg(mark_colour),
    ));
    right.push(Span::styled(
        truncate(&app.status.to_uppercase(), area.width as usize / 2),
        Style::default()
            .fg(if app.busy { theme.text } else { theme.muted })
            .add_modifier(Modifier::BOLD),
    ));

    frame.render_widget(
        Paragraph::new(Text::from(split_line(left, right, area.width)))
            .style(Style::default().bg(theme.panel)),
        area,
    );
}

/// Draw the instrument rail, on a terminal wide enough to hold it.
fn draw_rail(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.grid))
        .style(Style::default().bg(theme.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<RatLine> = Vec::new();
    let label = |text: &str| {
        RatLine::from(Span::styled(
            text.to_uppercase(),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let value = |text: String| RatLine::from(Span::styled(text, Style::default().fg(theme.text)));

    lines.push(RatLine::from(vec![
        Span::styled("  ◆ ", Style::default().fg(theme.accent)),
        Span::styled(
            "L I A",
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(RatLine::from(Span::styled(
        "    local intelligence",
        Style::default().fg(theme.muted),
    )));
    lines.push(RatLine::from(Span::styled(
        "    ─────────────────",
        Style::default().fg(theme.grid),
    )));
    lines.push(RatLine::from(""));

    lines.push(label("  connection"));
    lines.push(RatLine::from(vec![
        Span::styled(
            if app.busy { "  ◐ " } else { "  ● " },
            Style::default().fg(if app.busy {
                live_colour(theme, app.tick)
            } else {
                theme.ok
            }),
        ),
        Span::styled(
            if app.busy { "working" } else { "ready" },
            Style::default().fg(theme.text),
        ),
    ]));
    lines.push(RatLine::from(""));

    lines.push(label("  model"));
    lines.push(value(format!(
        "  {}",
        truncate(&app.config.effective_model(), 23)
    )));
    lines.push(RatLine::from(Span::styled(
        format!("  {}", truncate(app.config.provider.as_str(), 23)),
        Style::default().fg(theme.muted),
    )));
    lines.push(RatLine::from(""));

    lines.push(label("  session"));
    lines.push(value(format!("  {}", truncate(&app.session_id, 23))));

    if let Some(usage) = app.last_usage {
        lines.push(RatLine::from(""));
        lines.push(label("  context"));
        lines.push(RatLine::from(vec![
            Span::styled("  ↑ ", Style::default().fg(theme.ok)),
            Span::styled(
                format!("{}", usage.input_tokens),
                Style::default().fg(theme.text),
            ),
            Span::styled("  ↓ ", Style::default().fg(theme.heading)),
            Span::styled(
                format!("{}", usage.output_tokens),
                Style::default().fg(theme.text),
            ),
        ]));
    }

    if !app.plan.is_empty() {
        lines.push(RatLine::from(""));
        let done = app
            .plan
            .iter()
            .filter(|item| item.status == TodoStatus::Done)
            .count();
        lines.push(label(&format!("  tasks  {done}/{}", app.plan.len())));
        for item in app.plan.iter().take(4) {
            let (mark, colour) = match item.status {
                TodoStatus::Done => ("✓", theme.ok),
                TodoStatus::InProgress => {
                    (SPINNER[(app.tick as usize) % SPINNER.len()], theme.accent)
                }
                TodoStatus::Pending => ("○", theme.muted),
            };
            lines.push(RatLine::from(vec![
                Span::styled(format!("  {mark} "), Style::default().fg(colour)),
                Span::styled(
                    truncate(&clean(&item.content), 21),
                    Style::default().fg(theme.text),
                ),
            ]));
        }
    }

    lines.push(RatLine::from(""));
    lines.push(label("  keys"));
    lines.push(RatLine::from(Span::styled(
        "  enter  send",
        Style::default().fg(theme.muted),
    )));
    lines.push(RatLine::from(Span::styled(
        "  f1     commands",
        Style::default().fg(theme.muted),
    )));
    lines.push(RatLine::from(Span::styled(
        "  f2     voice",
        Style::default().fg(theme.muted),
    )));

    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Draw the list of things the assistant is working through.
fn draw_plan(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let done = app
        .plan
        .iter()
        .filter(|item| item.status == TodoStatus::Done)
        .count();
    let title = format!(" TASKS · {done}/{} done ", app.plan.len());

    let mut lines: Vec<RatLine> = Vec::new();
    for item in app.plan.iter().take(5) {
        let (mark, color) = match item.status {
            TodoStatus::Done => ("✓", theme.ok),
            TodoStatus::InProgress => (SPINNER[(app.tick as usize) % SPINNER.len()], theme.accent),
            TodoStatus::Pending => ("○", theme.muted),
        };
        lines.push(RatLine::from(vec![
            Span::styled(format!(" {mark} "), Style::default().fg(color)),
            Span::styled(clean(&item.content), Style::default().fg(theme.text)),
        ]));
    }
    if app.plan.len() > 5 {
        lines.push(RatLine::from(Span::styled(
            format!("   … and {} more", app.plan.len() - 5),
            Style::default().fg(theme.muted),
        )));
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .title(title)
        .title_style(Style::default().fg(theme.accent))
        .border_style(Style::default().fg(theme.frame));

    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

/// Draw the live activity rail.
///
/// This is the part of the interface that makes the work visible: an orbit
/// that turns, a waveform that moves, and the action itself, named.
fn draw_activity(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let tick = app.tick as usize;
    let spin = ORBIT[tick % ORBIT.len()];
    let elapsed = app.action_elapsed();

    let mut left = vec![
        Span::styled(
            format!(" {spin} "),
            Style::default()
                .fg(live_colour(theme, app.tick))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "WORKING",
            Style::default()
                .fg(theme.void)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            clean(&app.current_action()),
            Style::default().fg(theme.text),
        ),
    ];

    // A little travelling wave, so a long tool call still looks alive.
    let wave: String = (0..8)
        .map(|index| {
            let level = [0usize, 1, 3, 5, 3, 1, 0, 1][(index + tick) % 8];
            PULSE[level.min(PULSE.len() - 1)]
        })
        .collect();
    left.push(Span::styled(
        format!("  {wave}"),
        Style::default().fg(theme.heading),
    ));

    let right = vec![
        Span::styled(
            format!("{:02}:{:04.1}", (elapsed / 60.0) as u64, elapsed % 60.0),
            Style::default().fg(theme.muted),
        ),
        Span::styled(" ", Style::default()),
    ];

    frame.render_widget(
        Paragraph::new(Text::from(split_line(left, right, area.width)))
            .style(Style::default().bg(theme.sel)),
        area,
    );
}

/// Draw the scrolling transcript.
fn draw_transcript(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    // A little breathing room on either side, without changing the height
    // of anything. On a narrow terminal every cell counts.
    let margin = if area.width >= 54 { 1 } else { 0 };
    let content = Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin * 2),
        height: area.height,
    };
    let width = content.width.max(1) as usize;
    let mut lines: Vec<RatLine> = Vec::new();

    for entry in &app.transcript {
        draw_entry(&mut lines, entry, width, app);
    }

    // The turn in progress.
    for tool in &app.current_tools {
        draw_live_entry(&mut lines, tool, width, app);
    }
    if !app.thinking.is_empty() {
        if app.show_thinking {
            let mut text = app.thinking.clone();
            if app.busy {
                text.push('▋');
            }
            push_text(
                &mut lines,
                &text,
                width,
                "◇ ",
                theme.muted,
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
            );
        } else {
            push_text(
                &mut lines,
                "Thought",
                width,
                "◇ ",
                theme.muted,
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::ITALIC),
            );
        }
    }
    if !app.streaming.is_empty() {
        let mut text = app.streaming.clone();
        if app.busy {
            text.push('▋');
        }
        push_text(
            &mut lines,
            &text,
            width,
            "◆ ",
            theme.accent,
            Style::default().fg(theme.assistant),
        );
    }

    // The welcome mat, when there is no conversation yet.
    if lines.is_empty() {
        lines.push(RatLine::from(vec![
            Span::styled("  ◆ ", Style::default().fg(theme.accent)),
            Span::styled(
                "LIA",
                Style::default()
                    .fg(theme.heading)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  local-first agent", Style::default().fg(theme.muted)),
        ]));
        lines.push(RatLine::from(Span::styled(
            "    type a message to begin",
            Style::default().fg(theme.text),
        )));
        lines.push(RatLine::from(Span::styled(
            "    /help for commands · F2 to speak",
            Style::default().fg(theme.muted),
        )));
    }

    // Every row was wrapped to the width of the panel, so the height is
    // known exactly.
    let total = lines.len();

    let paragraph = Paragraph::new(Text::from(lines));

    // The transcript is anchored to a row: `app.scroll` is the index of
    // the first row on screen. While the view follows the newest output
    // it sits at the bottom, so new words push the older ones up; once
    // the person scrolls back, the view stays where it is put and the
    // new words arrive below it.
    let inner_height = content.height as usize;
    let max_scroll = total.saturating_sub(inner_height);
    // A panel that grew — the message box opened, the plan appeared —
    // takes rows away from the transcript. That is not the person asking
    // for the bottom, so it must not pin the view there.
    let shrank = max_scroll < app.scroll_max;
    app.scroll_max = max_scroll;
    app.view_rows = inner_height;

    let wanted = app.scroll;
    let offset = if app.follow {
        max_scroll
    } else {
        wanted.min(max_scroll)
    };
    app.scroll = offset;
    // The person scrolled to the bottom: the view follows the newest
    // output again.
    if !app.follow && !shrank && wanted >= max_scroll {
        app.follow = true;
    }

    let offset = u16::try_from(offset).unwrap_or(u16::MAX);
    frame.render_widget(paragraph.scroll((offset, 0)), content);
}

/// Draw one committed entry from the transcript.
fn draw_entry(rows: &mut Vec<RatLine>, entry: &Line, width: usize, app: &App) {
    let theme = app.theme;
    match entry {
        Line::User(text) => {
            push_text(
                rows,
                &format!(" {}", clean(text)),
                width,
                "❯ ",
                theme.user,
                Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
            );
        }
        Line::Assistant(text) => {
            push_text(
                rows,
                text,
                width,
                "◆ ",
                theme.accent,
                Style::default().fg(theme.assistant),
            );
        }
        Line::Thinking(text) => {
            // The model's own reasoning, kept apart from its answer. It is
            // hidden unless the reader asked for it, the way a thought is.
            if !app.show_thinking {
                push_text(
                    rows,
                    "Thought",
                    width,
                    "◇ ",
                    theme.muted,
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC),
                );
                let _ = text;
            } else {
                push_text(
                    rows,
                    text,
                    width,
                    "◇ ",
                    theme.muted,
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC),
                );
            }
        }
        Line::Tool {
            name,
            input,
            result,
            ..
        } => {
            push_tool(rows, name, input, result.as_ref(), width, app);
        }
        Line::Sub { label, note } => {
            let what = if label.is_empty() {
                "the other assistant".to_string()
            } else {
                format!("the other assistant ({label})")
            };
            push_text(
                rows,
                &format!("{what} {note}"),
                width,
                "↳ ",
                theme.muted,
                Style::default().fg(theme.muted),
            );
        }
        Line::Notice(text) => {
            push_text(
                rows,
                &clean(text).to_string(),
                width,
                "· ",
                theme.info,
                Style::default().fg(theme.info),
            );
        }
        Line::Error(text) => {
            push_text(
                rows,
                &clean(text).to_string(),
                width,
                "✗ ",
                theme.err,
                Style::default().fg(theme.err),
            );
        }
        Line::Blank => rows.push(RatLine::from("")),
    }
}

/// Draw an entry that is still happening.
fn draw_live_entry(rows: &mut Vec<RatLine>, entry: &Line, width: usize, app: &App) {
    let theme = app.theme;
    match entry {
        Line::Sub { label, note } => {
            let what = if label.is_empty() {
                "the other assistant".to_string()
            } else {
                format!("the other assistant ({label})")
            };
            push_text(
                rows,
                &format!("{what} {note}"),
                width,
                "↳ ",
                theme.muted,
                Style::default().fg(theme.muted),
            );
        }
        Line::Tool {
            name,
            input,
            result,
            ..
        } => {
            push_tool(rows, name, input, result.as_ref(), width, app);
        }
        other => draw_entry(rows, other, width, app),
    }
}

/// Draw a tool call as a compact card.
///
/// The card has a labelled spine, the arguments, and the beginning of the
/// result. A running tool keeps a spinner in its header; a finished one
/// carries a quiet verdict.
fn push_tool(
    rows: &mut Vec<RatLine>,
    name: &str,
    input: &str,
    result: Option<&super::app::ToolResultView>,
    width: usize,
    app: &App,
) {
    let theme = app.theme;
    let running = result.is_none();
    let (mark, colour, verdict) = match result {
        Some(result) if result.is_error => ("✗", theme.err, "FAILED"),
        Some(_) => ("✓", theme.ok, "DONE"),
        None => (
            SPINNER[(app.tick as usize) % SPINNER.len()],
            theme.accent,
            "RUNNING",
        ),
    };

    rows.push(RatLine::from(vec![
        Span::styled("  ╭─ ", Style::default().fg(theme.frame)),
        Span::styled(
            format!("{mark} {name}"),
            Style::default()
                .fg(if running { theme.tool } else { colour })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {verdict}"), Style::default().fg(colour)),
    ]));

    let gutter = "  │ ";
    if !input.is_empty() {
        for line in clean(input).lines() {
            push_text(
                rows,
                line,
                width,
                gutter,
                theme.frame,
                Style::default().fg(theme.muted),
            );
        }
    }
    if let Some(result) = result {
        let colour = if result.is_error {
            theme.err
        } else {
            theme.muted
        };
        let content = clean(&result.content);
        for line in content.lines().take(12) {
            push_text(
                rows,
                line,
                width,
                gutter,
                theme.frame,
                Style::default().fg(colour),
            );
        }
        if content.lines().count() > 12 {
            rows.push(RatLine::from(vec![
                Span::styled("  │ ", Style::default().fg(theme.frame)),
                Span::styled("…", Style::default().fg(theme.muted)),
            ]));
        }
        rows.push(RatLine::from(Span::styled(
            "  ╰─",
            Style::default().fg(theme.frame),
        )));
    } else {
        rows.push(RatLine::from(vec![
            Span::styled("  │ ", Style::default().fg(theme.frame)),
            Span::styled("working…", Style::default().fg(theme.muted)),
        ]));
    }
}

/// Add the rows of a piece of text, with a mark at the start of the first.
///
/// The text is wrapped to the width of the panel, and the rows below the
/// first are lined up under it, so the gutter stays straight.
fn push_text(
    rows: &mut Vec<RatLine>,
    text: &str,
    width: usize,
    first: &str,
    color: Color,
    style: Style,
) {
    let mark = UnicodeWidthStr::width(first);
    let text_width = width.saturating_sub(mark).max(1);
    let mark_style = Style::default().fg(color);
    for (index, row) in wrap::wrap(text, text_width).iter().enumerate() {
        let lead = if index == 0 {
            Span::styled(first.to_string(), mark_style)
        } else {
            Span::styled(" ".repeat(mark), mark_style)
        };
        rows.push(RatLine::from(vec![
            lead,
            Span::styled(row.text.clone(), style),
        ]));
    }
}

/// Draw the slash-command preview above the message box.
///
/// The preview floats: it does not take a row from the transcript, so the
/// cursor and the layout stay still while the person is deciding. It is
/// deliberately a list rather than a sentence, because the names are what
/// have to be scanned.
fn draw_command_preview(frame: &mut ratatui::Frame, input: Rect, app: &App) {
    let matches = app.command_matches();
    if matches.is_empty() || input.y < 4 || input.width < 24 {
        return;
    }

    let room = input.y.saturating_sub(1) as usize;
    let rows = matches.len().min(7).min(room.saturating_sub(2));
    if rows == 0 {
        return;
    }

    let height = (rows + 2) as u16;
    let width = input.width.saturating_sub(4).min(88);
    let x = input.x + (input.width.saturating_sub(width)) / 2;
    let y = input.y.saturating_sub(height + 1);
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let theme = app.theme;
    let selected_index = app.command_index % matches.len();
    // Keep the highlight in the window when there are more matches than
    // rows, so walking down the list never selects something invisible.
    let first = if selected_index >= rows {
        selected_index + 1 - rows
    } else {
        0
    };
    let mut lines: Vec<RatLine> = Vec::new();
    for (offset, command) in matches.iter().skip(first).take(rows).enumerate() {
        let selected = offset == selected_index.saturating_sub(first);
        let marker = if selected { "▸ " } else { "  " };
        let marker_style = Style::default().fg(if selected { theme.accent } else { theme.muted });
        let name_style = if selected {
            Style::default()
                .fg(theme.accent)
                .bg(theme.sel)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        let desc_style = Style::default().fg(if selected { theme.text } else { theme.muted });
        let desc_width = width.saturating_sub(2 + 2 + 15) as usize;
        lines.push(RatLine::from(vec![
            Span::styled(marker, marker_style),
            Span::styled(format!("{:<15}", command.name), name_style),
            Span::styled(
                truncate(command.description, desc_width.max(8)),
                desc_style.patch(Style::default().bg(if selected {
                    theme.sel
                } else {
                    theme.panel
                })),
            ),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.frame))
        .style(Style::default().bg(theme.panel))
        .title(Span::styled(
            format!(" COMMANDS · {} ", matches.len()),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " ↑↓ select · tab complete · enter run ",
            Style::default().fg(theme.muted),
        ));

    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
}

/// Draw the message box.
fn draw_input(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let theme = app.theme;

    // A picture waiting to go out says so on the box it waits in.
    let mut title = " MESSAGE ".to_string();
    if !app.pending_images.is_empty() {
        title = format!(" MESSAGE · {} image(s) ", app.pending_images.len());
    }
    // While the microphone is open, the box says so.
    if let super::app::VoiceState::Listening { .. } = app.voice_state {
        title = if app.live {
            let share = match app.live_share {
                crate::config::LiveShare::None => String::new(),
                other => format!(" · {}", other.as_str()),
            };
            format!(" ● LIVE{share} · {:.1}s ", app.recording_elapsed())
        } else {
            format!(" ● REC · {:.1}s ", app.recording_elapsed())
        };
    }
    let hint = " enter send · ctrl-j line · F2 voice · /help ";

    let border = if app.busy { theme.frame } else { theme.accent };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(theme.panel))
        .title(Span::styled(
            title,
            Style::default()
                .fg(if app.busy { theme.muted } else { theme.accent })
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(hint, Style::default().fg(theme.muted)));

    let (prompt, prompt_style) = if app.busy {
        (
            format!("{} ", ORBIT[(app.tick as usize) % ORBIT.len()]),
            Style::default().fg(theme.warn),
        )
    } else {
        ("❯ ".to_string(), Style::default().fg(theme.accent))
    };

    // The text is wrapped by hand, so the cursor can be put exactly where
    // the next character will appear.
    let inner = text_width(area.width);
    app.input_width = inner as u16;
    let lines = wrap::wrap(app.input.text(), inner);
    let visible = area.height.saturating_sub(2) as usize;
    let spot = wrap::cursor_spot(&lines, app.input.cursor());
    let offset = wrap::scroll_offset(lines.len(), visible, spot.line);

    let mut rows: Vec<RatLine> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index < offset {
            continue;
        }
        if index >= offset + visible {
            break;
        }
        // The prompt is on the first row; the rest of the message is lined
        // up under it.
        let gutter = if index == 0 {
            Span::styled(prompt.clone(), prompt_style)
        } else {
            Span::styled("  ", prompt_style)
        };
        rows.push(RatLine::from(vec![
            gutter,
            Span::styled(line.text.clone(), Style::default().fg(theme.text)),
        ]));
    }

    frame.render_widget(Paragraph::new(Text::from(rows)).block(block), area);

    // Put the cursor where it belongs.
    let text_x = area.x + 3;
    let text_y = area.y + 1;
    let cursor_y = text_y + spot.line.saturating_sub(offset) as u16;
    let cursor_x = text_x + spot.column as u16;
    if cursor_y < area.y + area.height && cursor_x < area.x + area.width {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Draw the bottom bar: what has been used, what is happening, what to press.
fn draw_footer(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let theme = app.theme;

    let mut left: Vec<Span> = Vec::new();
    if let Some(usage) = app.last_usage {
        left.push(Span::styled(
            format!("↑{} ↓{}  ", usage.input_tokens, usage.output_tokens),
            Style::default().fg(theme.muted),
        ));
    }
    if app.busy {
        let spin = SPINNER[(app.tick as usize) % SPINNER.len()];
        left.push(Span::styled(
            format!("{spin} "),
            Style::default().fg(theme.accent),
        ));
    }
    left.push(Span::styled(
        app.status.to_string(),
        Style::default().fg(theme.text),
    ));

    // How far back the view is. Without it the view looks stuck rather
    // than scrolled.
    let back = app.scroll_max.saturating_sub(app.scroll);
    if back > 0 {
        left.push(Span::styled(
            format!("  ▲ {back} rows"),
            Style::default().fg(theme.warn),
        ));
    }

    // While the microphone is open, the meter is on the right.
    if let super::app::VoiceState::Listening { .. } = app.voice_state {
        let right = vec![
            Span::styled(
                if app.live { "● LIVE " } else { "● REC " },
                Style::default().fg(theme.err),
            ),
            Span::styled(
                format!("{:.1}s ", app.recording_elapsed()),
                Style::default().fg(theme.text),
            ),
            Span::styled(meter(&app.voice_levels), Style::default().fg(theme.accent)),
        ];
        frame.render_widget(
            Paragraph::new(Text::from(split_line(left, right, area.width)))
                .style(Style::default().bg(theme.panel)),
            area,
        );
        return;
    }

    let right = match &app.popup {
        Popup::Approval(_) => vec![Span::styled(
            "y approve · n deny · a always",
            Style::default().fg(theme.warn),
        )],
        Popup::Sessions => vec![Span::styled(
            "↑↓ move · enter open · esc close",
            Style::default().fg(theme.muted),
        )],
        Popup::Help => vec![Span::styled("esc close", Style::default().fg(theme.muted))],
        Popup::None => vec![Span::styled(
            "⏎ send · ctrl-j newline · F2 talk · /help",
            Style::default().fg(theme.muted),
        )],
    };

    frame.render_widget(
        Paragraph::new(Text::from(split_line(left, right, area.width)))
            .style(Style::default().bg(theme.panel)),
        area,
    );
}

/// The level meter, as a row of blocks.
fn meter(levels: &[f32]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    levels
        .iter()
        .map(|level| {
            let index = ((level.clamp(0.0, 1.0) * 7.0).round() as usize).min(BARS.len() - 1);
            BARS[index]
        })
        .collect()
}

/// Draw the approval prompt.
fn draw_approval(
    frame: &mut ratatui::Frame,
    area: Rect,
    pending: &super::app::PendingApproval,
    theme: Theme,
) {
    let popup = centered(area, 72, 46);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        RatLine::from(Span::styled(
            "⚠  PERMISSION REQUIRED",
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        )),
        RatLine::from(""),
        RatLine::from(vec![
            Span::styled("  tool  ", Style::default().fg(theme.muted)),
            Span::styled(
                pending.request.tool.clone(),
                Style::default().fg(theme.tool).add_modifier(Modifier::BOLD),
            ),
        ]),
        RatLine::from(vec![
            Span::styled("  why   ", Style::default().fg(theme.muted)),
            Span::styled(
                pending.request.summary.clone(),
                Style::default().fg(theme.text),
            ),
        ]),
        RatLine::from(""),
    ];

    for line in pending.request.arguments.lines().take(15) {
        lines.push(RatLine::from(Span::styled(
            format!("  {line}"),
            Style::default().fg(theme.muted),
        )));
    }

    lines.push(RatLine::from(""));
    lines.push(RatLine::from(vec![
        Span::styled("  [y]", Style::default().fg(theme.ok).bold()),
        Span::styled(" approve    ", Style::default().fg(theme.muted)),
        Span::styled("  [n]", Style::default().fg(theme.err).bold()),
        Span::styled(" deny    ", Style::default().fg(theme.muted)),
        Span::styled("  [a]", Style::default().fg(theme.accent).bold()),
        Span::styled(" always", Style::default().fg(theme.muted)),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.warn))
        .style(Style::default().bg(theme.panel))
        .title(Span::styled(
            " approval ",
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Draw the session list.
fn draw_sessions(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let popup = centered(area, 72, 56);
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    for (index, (id, title)) in app.sessions.iter().enumerate() {
        let marker = if *id == app.session_id {
            "● "
        } else {
            "○ "
        };
        let selected = index == app.session_index;
        let title_style = if selected {
            Style::default().fg(theme.accent).bold()
        } else {
            Style::default().fg(theme.text)
        };
        let mut spans = vec![
            Span::styled(
                if selected { "▌ " } else { "  " },
                Style::default().fg(theme.accent),
            ),
            Span::styled(marker, Style::default().fg(theme.ok)),
            Span::styled(truncate(title, 34), title_style),
            Span::styled(format!("  {id}"), Style::default().fg(theme.muted)),
        ];
        if selected {
            spans = spans
                .into_iter()
                .map(|s| s.patch_style(Style::default().bg(theme.sel)))
                .collect();
        }
        lines.push(RatLine::from(spans));
    }
    if lines.is_empty() {
        lines.push(RatLine::from(Span::styled(
            "  no sessions yet",
            Style::default().fg(theme.muted),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.panel))
        .title(Span::styled(
            " sessions ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Draw the help overlay.
///
/// The commands and the keys sit side by side, so the whole map fits on an
/// ordinary terminal without scrolling. The descriptions live in the live
/// preview, where there is room for them.
fn draw_help(frame: &mut ratatui::Frame, area: Rect, theme: Theme) {
    let popup = centered(area, 92, 74);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.panel))
        .title(Span::styled(
            " help ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(block.clone(), popup);

    let inner = block.inner(popup);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Length(1),
            Constraint::Percentage(50),
        ])
        .split(inner);

    let heading = |text: &str| {
        RatLine::from(Span::styled(
            text.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
    };

    let mut commands: Vec<RatLine> = vec![heading("◆ COMMANDS"), RatLine::from("")];
    for command in commands::COMMANDS {
        let label = if command.args.is_empty() {
            command.name.to_string()
        } else {
            format!("{} {}", command.name, command.args)
        };
        commands.push(RatLine::from(Span::styled(
            label,
            Style::default().fg(theme.text),
        )));
    }

    let keys = vec![
        heading("◆ KEYS"),
        RatLine::from(""),
        RatLine::from("ctrl-c     quit"),
        RatLine::from("⏎          send · ctrl-j newline"),
        RatLine::from("↑↓         lines, then history"),
        RatLine::from("ctrl-p/n   history"),
        RatLine::from("ctrl-←/→   one word"),
        RatLine::from("home/end   line · ctrl: message"),
        RatLine::from("ctrl-u/k/w cut"),
        RatLine::from("F1         this help"),
        RatLine::from("F2         record the microphone"),
        RatLine::from("wheel      scroll"),
        RatLine::from("page up/down  half a page"),
        RatLine::from("alt-home/end  oldest / newest"),
        RatLine::from("esc        close a popup"),
    ];

    frame.render_widget(Paragraph::new(Text::from(commands)), columns[0]);
    frame.render_widget(
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(theme.grid)),
        columns[1],
    );
    frame.render_widget(Paragraph::new(Text::from(keys)), columns[2]);
}

/// A rectangle centred in `area`, sized as a percentage of it.
fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
