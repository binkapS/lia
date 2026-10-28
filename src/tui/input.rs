//! The text being written in the message box.
//!
//! The buffer holds a whole message, which may take several lines. The
//! cursor is a *character* index, never a byte offset, so it always sits on
//! a character boundary and every movement is one character wide. The old
//! buffer mixed the two, and deleted a single byte — which panicked the
//! interface on any character that takes more than one byte.

/// The text being written, and where the cursor is in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// The text. A line break is a `\n`.
    text: String,
    /// Where the cursor is, as a character index into `text`.
    cursor: usize,
    /// The column the cursor would like to be in, when moving up and down.
    /// `None` means "wherever it was".
    goal_column: Option<usize>,
}

impl Input {
    /// An empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The text as it stands.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether there is nothing in the buffer.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// How many characters the text holds.
    pub fn len_chars(&self) -> usize {
        self.text.chars().count()
    }

    /// Where the cursor is, as a character index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The column the cursor would like to be in, when moving up and down.
    pub fn goal_column(&self) -> Option<usize> {
        self.goal_column
    }

    /// Remember the column the cursor would like to be in.
    pub fn set_goal_column(&mut self, column: Option<usize>) {
        self.goal_column = column;
    }

    /// Replace the text and put the cursor at the end of it.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.len_chars();
        self.goal_column = None;
    }

    /// Replace the text and put the cursor at a character index.
    pub fn set_text_at(&mut self, text: &str, cursor: usize) {
        self.text = text.to_string();
        self.cursor = cursor.min(self.len_chars());
        self.goal_column = None;
    }

    /// Put the cursor at a character index.
    pub fn set_cursor_at(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.len_chars());
    }

    /// Empty the buffer.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.goal_column = None;
    }

    /// The trimmed text, and an empty buffer.
    pub fn take(&mut self) -> String {
        let text = self.text.trim().to_string();
        self.clear();
        text
    }

    /// Put a character in at the cursor. A newline makes a line break.
    pub fn insert_char(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
        self.goal_column = None;
    }

    /// Put a run of text in at the cursor — a paste. Newlines in it make
    /// line breaks, so a pasted paragraph stays a paragraph.
    pub fn insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let at = self.byte_at(self.cursor);
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
        self.goal_column = None;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        // The cursor is a character index, so the character before it starts
        // at the byte offset of the character before *that*.
        let start = self.byte_at(self.cursor - 1);
        let end = self.byte_at(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
        self.goal_column = None;
        true
    }

    /// Delete the character at the cursor.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.len_chars() {
            return false;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.text.replace_range(start..end, "");
        self.goal_column = None;
        true
    }

    /// Move one character to the left.
    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.goal_column = None;
        true
    }

    /// Move one character to the right.
    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.len_chars() {
            return false;
        }
        self.cursor += 1;
        self.goal_column = None;
        true
    }

    /// Move to the start of the word before the cursor.
    pub fn move_word_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let mut index = self.cursor;
        while index > 0 && chars[index - 1].is_whitespace() {
            index -= 1;
        }
        while index > 0 && !chars[index - 1].is_whitespace() {
            index -= 1;
        }
        if index == self.cursor {
            return false;
        }
        self.cursor = index;
        self.goal_column = None;
        true
    }

    /// Move to the start of the word after the cursor.
    pub fn move_word_right(&mut self) -> bool {
        let chars: Vec<char> = self.text.chars().collect();
        if self.cursor >= chars.len() {
            return false;
        }
        let mut index = self.cursor;
        while index < chars.len() && !chars[index].is_whitespace() {
            index += 1;
        }
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index == self.cursor {
            return false;
        }
        self.cursor = index;
        self.goal_column = None;
        true
    }

    /// Move to the start of the line the cursor is on.
    pub fn move_to_line_start(&mut self) {
        self.cursor = self.line_start_index();
        self.goal_column = None;
    }

    /// Move to the end of the line the cursor is on.
    pub fn move_to_line_end(&mut self) {
        self.cursor = self.line_end_index();
        self.goal_column = None;
    }

    /// Move to the start of the whole text.
    pub fn move_to_buffer_start(&mut self) {
        self.cursor = 0;
        self.goal_column = None;
    }

    /// Move to the end of the whole text.
    pub fn move_to_buffer_end(&mut self) {
        self.cursor = self.len_chars();
        self.goal_column = None;
    }

    /// Delete from the cursor back to the start of the line.
    pub fn kill_to_line_start(&mut self) -> bool {
        let start = self.line_start_index();
        if start == self.cursor {
            return false;
        }
        let from = self.byte_at(start);
        let to = self.byte_at(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor = start;
        self.goal_column = None;
        true
    }

    /// Delete from the cursor to the end of the line.
    pub fn kill_to_line_end(&mut self) -> bool {
        let end = self.line_end_index();
        if end == self.cursor {
            return false;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(end);
        self.text.replace_range(from..to, "");
        self.goal_column = None;
        true
    }

    /// Delete the word before the cursor.
    pub fn kill_word_backward(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start == self.cursor {
            return false;
        }
        let from = self.byte_at(start);
        let to = self.byte_at(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor = start;
        self.goal_column = None;
        true
    }

    /// Delete the word at the cursor.
    pub fn kill_word_forward(&mut self) -> bool {
        let chars: Vec<char> = self.text.chars().collect();
        if self.cursor >= chars.len() {
            return false;
        }
        let mut end = self.cursor;
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        if end == self.cursor {
            return false;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(end);
        self.text.replace_range(from..to, "");
        self.goal_column = None;
        true
    }

    /// Which line the cursor is on, and how far along it is.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0usize;
        let mut column = 0usize;
        for (index, ch) in self.text.chars().enumerate() {
            if index == self.cursor {
                break;
            }
            if ch == '\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }
        (line, column)
    }

    /// The character index where the given line starts.
    pub fn line_start(&self, line: usize) -> usize {
        let mut current = 0usize;
        for (index, ch) in self.text.chars().enumerate() {
            if ch == '\n' {
                if current == line {
                    return index + 1;
                }
                current += 1;
            }
        }
        self.len_chars()
    }

    /// The character index of the start of the line the cursor is on.
    fn line_start_index(&self) -> usize {
        let mut start = 0usize;
        for (index, ch) in self.text.chars().enumerate() {
            if index >= self.cursor {
                break;
            }
            if ch == '\n' {
                start = index + 1;
            }
        }
        start
    }

    /// The character index of the end of the line the cursor is on.
    fn line_end_index(&self) -> usize {
        let mut end = self.cursor;
        for (index, ch) in self.text.chars().enumerate() {
            if index < self.cursor {
                continue;
            }
            if ch == '\n' {
                return index;
            }
            end = index + 1;
        }
        end
    }

    /// The byte offset of the character at a character index.
    fn byte_at(&self, index: usize) -> usize {
        self.text
            .char_indices()
            .nth(index)
            .map(|(offset, _)| offset)
            .unwrap_or(self.text.len())
    }
}
