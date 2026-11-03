//! Documents the assistant can create, read, edit and style: PDF and DOCX.
//!
//! A document is described by a small, validated JSON spec (headings,
//! paragraphs with styled runs, lists, tables, page breaks) that both the
//! DOCX and PDF writers render. Reading and editing work on the real files:
//! DOCX edits change only the paragraphs they target and keep everything
//! else byte-for-byte; PDF edits are the few that are possible without a
//! layout engine (delete pages, best-effort text replacement, metadata,
//! appending generated pages).

pub mod docx;
pub mod pdf;
mod xml;

use serde::Deserialize;

/// Largest spec accepted, so one call cannot build an unbounded file.
pub const MAX_BLOCKS: usize = 2_000;
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TABLE_CELLS: usize = 10_000;

/// A reason a document could not be produced or read.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, DocError>;

pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(DocError::Invalid(message.into()))
}

/// Text alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// The look of a run of text or a whole paragraph. Unset fields inherit.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Style {
    /// `helvetica`, `times` or `courier` for PDF; any installed family name
    /// for DOCX (PDF maps unknown names to helvetica).
    pub font: Option<String>,
    /// Size in points.
    pub size: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    /// `RRGGBB` hex, with or without a leading `#`.
    pub color: Option<String>,
    pub align: Option<Align>,
}

impl Style {
    /// `self` over `base`: fields set here win.
    pub fn over(&self, base: &Style) -> Style {
        Style {
            font: self.font.clone().or_else(|| base.font.clone()),
            size: self.size.or(base.size),
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            underline: self.underline.or(base.underline),
            color: self.color.clone().or_else(|| base.color.clone()),
            align: self.align.or(base.align),
        }
    }

    /// Check values and normalise the colour.
    pub fn validate(&mut self) -> Result<()> {
        if let Some(size) = self.size
            && !(4.0..=200.0).contains(&size)
        {
            return invalid("size must be between 4 and 200 points");
        }
        if let Some(color) = &self.color {
            let hex = color.trim_start_matches('#');
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return invalid("color must be a 6-digit hex value like 1F4E79");
            }
            self.color = Some(hex.to_ascii_uppercase());
        }
        if let Some(font) = &self.font
            && (font.is_empty()
                || font.len() > 64
                || font.chars().any(|c| c.is_control() || c == '<' || c == '>'))
        {
            return invalid("font is not a valid family name");
        }
        Ok(())
    }
}

/// A piece of text with its own style.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Run {
    pub text: String,
    #[serde(flatten)]
    pub style: Style,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Run {
        Run {
            text: text.into(),
            style: Style::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawBlock {
    Heading {
        level: Option<u8>,
        text: Option<String>,
        runs: Option<Vec<Run>>,
        #[serde(flatten)]
        style: Style,
    },
    Paragraph {
        text: Option<String>,
        runs: Option<Vec<Run>>,
        #[serde(flatten)]
        style: Style,
    },
    List {
        #[serde(default)]
        ordered: bool,
        items: Vec<String>,
        #[serde(flatten)]
        style: Style,
    },
    Table {
        rows: Vec<Vec<String>>,
        #[serde(default = "yes")]
        header: bool,
    },
    PageBreak {},
}

fn yes() -> bool {
    true
}

fn into_runs(text: Option<String>, runs: Option<Vec<Run>>) -> Result<Vec<Run>> {
    match (text, runs) {
        (Some(_), Some(_)) => invalid("give either text or runs, not both"),
        (Some(text), None) => Ok(vec![Run::plain(text)]),
        (None, Some(runs)) => Ok(runs),
        (None, None) => invalid("a block needs text or runs"),
    }
}

/// One validated block of a document.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        runs: Vec<Run>,
        style: Style,
    },
    Paragraph {
        runs: Vec<Run>,
        style: Style,
    },
    List {
        ordered: bool,
        items: Vec<String>,
        style: Style,
    },
    Table {
        rows: Vec<Vec<String>>,
        header: bool,
    },
    PageBreak,
}

impl Block {
    /// Parse and validate blocks from JSON.
    pub fn parse_all(value: &serde_json::Value) -> Result<Vec<Block>> {
        let raw: Vec<RawBlock> = serde_json::from_value(value.clone())
            .map_err(|e| DocError::Invalid(format!("blocks are not valid: {e}")))?;
        if raw.len() > MAX_BLOCKS {
            return invalid(format!("at most {MAX_BLOCKS} blocks"));
        }
        let mut bytes = 0usize;
        let mut cells = 0usize;
        let mut blocks = Vec::with_capacity(raw.len());
        for block in raw {
            blocks.push(match block {
                RawBlock::Heading {
                    level,
                    text,
                    runs,
                    mut style,
                } => {
                    let level = level.unwrap_or(1);
                    if !(1..=3).contains(&level) {
                        return invalid("heading level must be 1, 2 or 3");
                    }
                    style.validate()?;
                    let runs = checked_runs(into_runs(text, runs)?)?;
                    count(&mut bytes, runs.iter().map(|r| r.text.len()).sum())?;
                    Block::Heading { level, runs, style }
                }
                RawBlock::Paragraph {
                    text,
                    runs,
                    mut style,
                } => {
                    style.validate()?;
                    let runs = checked_runs(into_runs(text, runs)?)?;
                    count(&mut bytes, runs.iter().map(|r| r.text.len()).sum())?;
                    Block::Paragraph { runs, style }
                }
                RawBlock::List {
                    ordered,
                    items,
                    mut style,
                } => {
                    style.validate()?;
                    if items.is_empty() {
                        return invalid("a list needs at least one item");
                    }
                    count(&mut bytes, items.iter().map(String::len).sum())?;
                    Block::List {
                        ordered,
                        items,
                        style,
                    }
                }
                RawBlock::Table { rows, header } => {
                    let width = rows.first().map(Vec::len).unwrap_or(0);
                    if width == 0 || width > 12 {
                        return invalid("a table needs 1 to 12 columns");
                    }
                    if rows.iter().any(|row| row.len() != width) {
                        return invalid("every table row must have the same number of cells");
                    }
                    cells += rows.len() * width;
                    if cells > MAX_TABLE_CELLS {
                        return invalid(format!("at most {MAX_TABLE_CELLS} table cells"));
                    }
                    count(
                        &mut bytes,
                        rows.iter().flatten().map(String::len).sum::<usize>(),
                    )?;
                    Block::Table { rows, header }
                }
                RawBlock::PageBreak {} => Block::PageBreak,
            });
        }
        Ok(blocks)
    }
}

fn count(total: &mut usize, add: usize) -> Result<()> {
    *total += add;
    if *total > MAX_TEXT_BYTES {
        return invalid("the document text is too large");
    }
    Ok(())
}

fn checked_runs(mut runs: Vec<Run>) -> Result<Vec<Run>> {
    if runs.is_empty() {
        return invalid("runs must not be empty");
    }
    for run in &mut runs {
        run.style.validate()?;
    }
    Ok(runs)
}

/// Page size names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PageSize {
    #[default]
    A4,
    Letter,
}

impl PageSize {
    /// Width and height in points.
    pub fn points(self) -> (f32, f32) {
        match self {
            PageSize::A4 => (595.0, 842.0),
            PageSize::Letter => (612.0, 792.0),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawSpec {
    title: Option<String>,
    author: Option<String>,
    #[serde(default)]
    blocks: serde_json::Value,
    #[serde(default)]
    style: Style,
    #[serde(default)]
    page_size: PageSize,
    /// Page margin in points.
    margin: Option<f32>,
    #[serde(default)]
    page_numbers: bool,
}

/// A whole document to render.
#[derive(Debug, Clone)]
pub struct Spec {
    pub title: Option<String>,
    pub author: Option<String>,
    pub blocks: Vec<Block>,
    /// The default look of body text.
    pub base: Style,
    pub page_size: PageSize,
    pub margin: f32,
    pub page_numbers: bool,
}

impl Spec {
    pub fn parse(value: &serde_json::Value) -> Result<Spec> {
        let raw: RawSpec = serde_json::from_value(value.clone())
            .map_err(|e| DocError::Invalid(format!("the document spec is not valid: {e}")))?;
        let mut base = raw.style;
        base.validate()?;
        let margin = raw.margin.unwrap_or(72.0);
        if !(18.0..=200.0).contains(&margin) {
            return invalid("margin must be between 18 and 200 points");
        }
        let blocks = if raw.blocks.is_null() {
            Vec::new()
        } else {
            Block::parse_all(&raw.blocks)?
        };
        if blocks.is_empty() && raw.title.is_none() {
            return invalid("a document needs a title or at least one block");
        }
        for text in [&raw.title, &raw.author].into_iter().flatten() {
            if text.len() > 500 {
                return invalid("title and author are limited to 500 bytes");
            }
        }
        Ok(Spec {
            title: raw.title,
            author: raw.author,
            blocks,
            base,
            page_size: raw.page_size,
            margin,
            page_numbers: raw.page_numbers,
        })
    }
}

/// Which kind of file a path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Pdf,
    Docx,
}

impl Format {
    pub fn of(path: &std::path::Path) -> Result<Format> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("pdf") => Ok(Format::Pdf),
            Some("docx") => Ok(Format::Docx),
            _ => Err(DocError::Unsupported(
                "only .pdf and .docx files are supported (convert older .doc files first)".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_spec_is_parsed_and_validated() {
        let spec = Spec::parse(&json!({
            "title": "Report",
            "style": {"font": "times", "size": 11, "color": "#1f4e79"},
            "blocks": [
                {"type": "heading", "level": 2, "text": "Intro", "align": "center"},
                {"type": "paragraph", "runs": [{"text": "bold", "bold": true}, {"text": " plain"}]},
                {"type": "list", "ordered": true, "items": ["a", "b"]},
                {"type": "table", "rows": [["h1", "h2"], ["1", "2"]]},
                {"type": "page_break"}
            ]
        }))
        .unwrap();
        assert_eq!(spec.blocks.len(), 5);
        assert_eq!(spec.base.color.as_deref(), Some("1F4E79"));
        assert!(matches!(spec.blocks[3], Block::Table { header: true, .. }));
    }

    #[test]
    fn bad_specs_are_rejected_with_a_reason() {
        for bad in [
            json!({"blocks": []}),
            json!({"title": "t", "blocks": [{"type": "heading", "level": 9, "text": "x"}]}),
            json!({"title": "t", "blocks": [{"type": "paragraph"}]}),
            json!({"title": "t", "blocks": [{"type": "paragraph", "text": "x", "color": "red"}]}),
            json!({"title": "t", "blocks": [{"type": "table", "rows": [["a"], ["b", "c"]]}]}),
            json!({"title": "t", "blocks": [{"type": "paragraph", "text": "x", "size": 2}]}),
            json!({"title": "t", "margin": 2}),
            json!({"title": "t", "blocks": [{"type": "nope"}]}),
        ] {
            assert!(Spec::parse(&bad).is_err(), "accepted {bad}");
        }
    }
}
