//! Wrapping the message box's text into the rows the terminal shows.
//!
//! The renderer draws the text and the interface puts the cursor in it, so
//! both need the same picture of where the lines fall. Doing the wrap here
//! — rather than letting the widget wrap it — means the box, the cursor and
//! the scroll all come from one description, and cannot drift apart.

use unicode_width::UnicodeWidthChar;

/// One row of the message box: a piece of the text that fits the width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualLine {
    /// The text of the row.
    pub text: String,
    /// The character index in the buffer where the row starts.
    pub start: usize,
    /// The character index in the buffer where the row ends, exclusive.
    ///
    /// The row's text is exactly the buffer's characters from `start` to
    /// `end`, so `end - start` is the number of characters in it.
    pub end: usize,
}

/// Where the cursor sits once the text is wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorSpot {
    /// Which row the cursor is on.
    pub line: usize,
    /// How many cells from the left of that row.
    pub column: usize,
}

/// Wrap `text` into rows that each fit in `width` cells.
///
/// The wrap happens on word boundaries; a word wider than the width is
/// broken at a cell boundary, so a long address does not run off the edge.
/// A line break in the text always starts a new row, and a text ending in a
/// line break has a trailing empty row, so the cursor can sit there.
pub fn wrap(text: &str, width: usize) -> Vec<VisualLine> {
    let limit = width.max(1);
    let chars: Vec<(usize, char)> = text.chars().enumerate().collect();
    let mut lines: Vec<VisualLine> = Vec::new();

    let mut start = 0usize;
    for index in 0..chars.len() {
        if chars[index].1 == '\n' {
            wrap_logical(&chars[start..index], start, limit, &mut lines);
            start = index + 1;
        }
    }
    wrap_logical(&chars[start..], start, limit, &mut lines);
    lines
}

/// Where the cursor (a character index) sits in the wrapped text.
pub fn cursor_spot(lines: &[VisualLine], cursor: usize) -> CursorSpot {
    if lines.is_empty() {
        return CursorSpot { line: 0, column: 0 };
    }
    // The row the cursor is on: the last one that starts at or before it.
    let mut index = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if line.start <= cursor {
            index = i;
        } else {
            break;
        }
    }
    let line = &lines[index];

    if cursor >= line.end {
        // The cursor is at the end of the row. A soft wrap puts the next
        // character on the row below, so the cursor goes there; a line
        // break keeps it on this row, at its end.
        if let Some(next) = lines.get(index + 1)
            && next.start == cursor
        {
            return CursorSpot {
                line: index + 1,
                column: 0,
            };
        }
        return CursorSpot {
            line: index,
            column: width_of(&line.text.chars().collect::<Vec<_>>()),
        };
    }

    let into = (cursor - line.start).min(line.text.chars().count());
    CursorSpot {
        line: index,
        column: width_of(&line.text.chars().take(into).collect::<Vec<_>>()),
    }
}

/// The character index in a row that is `cells` cells along it.
pub fn column_to_char(line: &VisualLine, cells: usize) -> usize {
    let mut used = 0usize;
    for (index, ch) in line.text.chars().enumerate() {
        if used >= cells {
            return line.start + index;
        }
        used += cell_width(ch);
    }
    line.start + line.text.chars().count()
}

/// How far down the box must scroll for the cursor to be visible.
///
/// The answer is the number of rows to skip at the top.
pub fn scroll_offset(total: usize, visible: usize, cursor_line: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    let last = total - visible;
    if cursor_line < visible {
        return 0;
    }
    // The cursor is the last row on show, unless the text ends before that.
    (cursor_line + 1 - visible).min(last)
}

/// How many cells a run of text takes.
fn width_of(chars: &[char]) -> usize {
    chars.iter().map(|c| cell_width(*c)).sum()
}

/// How many cells a character takes.
fn cell_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Wrap one logical line — the text between two line breaks.
///
/// The wrap happens on word boundaries: a word that does not fit moves to
/// the next row whole. A word that is longer than a row is broken at a cell
/// boundary, so a long address does not run off the edge.
fn wrap_logical(chars: &[(usize, char)], base: usize, limit: usize, lines: &mut Vec<VisualLine>) {
    let mut current = String::new();
    let mut current_start = base;
    let mut current_end = base;
    let mut width = 0usize;
    // The spaces that follow the text so far, held back so a break can be
    // made before a word instead of after it.
    let mut pending = String::new();
    let mut pending_width = 0usize;
    let mut pending_start = base;
    let mut pending_end = base;

    let mut index = 0usize;
    while index < chars.len() {
        // The word, and how wide it is.
        let word_start = index;
        let mut word_width = 0usize;
        while index < chars.len() && !chars[index].1.is_whitespace() {
            word_width += cell_width(chars[index].1);
            index += 1;
        }
        let word_end = index;

        if !current.is_empty() && width + pending_width + word_width > limit {
            // The word does not fit on this row. Close the row, keeping the
            // spaces that came before the word — unless they would make the
            // row too wide, in which case they are left out. A space at the
            // edge of a row cannot be seen anyway.
            let mut text = std::mem::take(&mut current);
            let mut end = current_end;
            if !pending.is_empty() && width + pending_width <= limit {
                text.push_str(&pending);
                end = pending_end;
            }
            lines.push(VisualLine {
                text,
                start: current_start,
                end,
            });
            width = 0;
            pending.clear();
            pending_width = 0;
        }

        if current.is_empty() {
            // A new row starts here. The indentation in front of it is
            // kept, so a block of code keeps its shape.
            current_start = if pending.is_empty() {
                chars[word_start].0
            } else {
                pending_start
            };
            current_end = current_start;
        }

        // The spaces, and then the word.
        current.push_str(&pending);
        width += pending_width;
        pending.clear();
        pending_width = 0;

        for (at, ch) in chars[word_start..word_end].iter().copied() {
            let w = cell_width(ch);
            if width + w > limit && width > 0 {
                // The word is longer than a row: it is broken.
                let text = std::mem::take(&mut current);
                lines.push(VisualLine {
                    text,
                    start: current_start,
                    end: current_end,
                });
                width = 0;
                current_start = at;
            }
            current.push(ch);
            width += w;
            current_end = at + 1;
        }

        // The spaces that follow the word.
        while index < chars.len() && chars[index].1.is_whitespace() {
            if pending.is_empty() {
                pending_start = chars[index].0;
            }
            pending.push(chars[index].1);
            pending_width += cell_width(chars[index].1);
            pending_end = chars[index].0 + 1;
            index += 1;
        }
    }

    current.push_str(&pending);
    let end = if pending.is_empty() {
        current_end
    } else {
        pending_end
    };
    lines.push(VisualLine {
        text: current,
        start: current_start,
        end,
    });
}
