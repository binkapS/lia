//! PDF creation, reading and editing on top of `lopdf`.
//!
//! Created files use the 14 standard PDF fonts, so nothing is embedded and
//! text outside Latin-1/WinAnsi is replaced with `?`. Editing an existing PDF
//! is limited to what the format allows without reflowing: deleting pages,
//! best-effort text replacement, metadata, and appending new pages.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use serde_json::Value;

use super::{Align, Block, DocError, PageSize, Result, Run, Spec, Style, invalid};

const MAX_PDF_PAGES: usize = 500;

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Helvetica,
    Times,
    Courier,
}

fn family_of(font: Option<&str>) -> Family {
    let name = font.unwrap_or("").to_ascii_lowercase();
    if name.contains("courier") || name.contains("mono") || name.contains("consolas") {
        Family::Courier
    } else if name.contains("sans") || name.contains("arial") || name.contains("helvetica") {
        Family::Helvetica
    } else if name.contains("times")
        || name.contains("serif")
        || name.contains("georgia")
        || name.contains("garamond")
        || name.contains("cambria")
    {
        Family::Times
    } else {
        Family::Helvetica
    }
}

fn font_index(family: Family, bold: bool, italic: bool) -> usize {
    let base = match family {
        Family::Helvetica => 0,
        Family::Times => 4,
        Family::Courier => 8,
    };
    base + usize::from(bold) + 2 * usize::from(italic)
}

const FONT_NAMES: [&str; 12] = [
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
];

/// Helvetica advance widths (per 1000 em) for ASCII 32..=126.
const HELV: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

fn char_width(c: char, font: usize) -> f32 {
    if font >= 8 {
        return 0.6;
    }
    let base = match c {
        ' '..='~' => f32::from(HELV[c as usize - 32]) / 1000.0,
        _ => 0.556,
    };
    let mut w = base;
    if font >= 4 {
        w *= 0.92;
    }
    if font % 2 == 1 {
        w *= 1.05;
    }
    w
}

fn to_win_ansi(c: char) -> u8 {
    match c {
        '\u{20}'..='\u{7e}' | '\u{a0}'..='\u{ff}' => c as u32 as u8,
        '€' => 0x80,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '™' => 0x99,
        '…' => 0x85,
        '\t' => b' ',
        _ => b'?',
    }
}

/// A PDF literal string made of printable ASCII and octal escapes.
fn literal(text: &str) -> String {
    let mut out = String::from("(");
    for c in text.chars() {
        let b = to_win_ansi(c);
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
    out
}

fn rgb(color: &Option<String>) -> (f32, f32, f32) {
    let Some(hex) = color else {
        return (0.0, 0.0, 0.0);
    };
    let part = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .map_or(0.0, |v| f32::from(v) / 255.0)
    };
    (part(0), part(2), part(4))
}

#[derive(Clone)]
struct Token {
    text: String,
    font: usize,
    size: f32,
    color: (f32, f32, f32),
    underline: bool,
    width: f32,
    /// Whether a space precedes this token on the line.
    gap: bool,
}

fn token_width(text: &str, font: usize, size: f32) -> f32 {
    text.chars().map(|c| char_width(c, font)).sum::<f32>() * size
}

fn tokens_of(runs: &[Run], base: &Style) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut pending_gap = false;
    for run in runs {
        let style = run.style.over(base);
        let family = family_of(style.font.as_deref());
        let font = font_index(
            family,
            style.bold.unwrap_or(false),
            style.italic.unwrap_or(false),
        );
        let size = style.size.unwrap_or(11.0);
        let color = rgb(&style.color);
        let underline = style.underline.unwrap_or(false);
        let mut word = String::new();
        let flush = |word: &mut String, gap: &mut bool, tokens: &mut Vec<Token>| {
            if !word.is_empty() {
                let width = token_width(word, font, size);
                tokens.push(Token {
                    text: std::mem::take(word),
                    font,
                    size,
                    color,
                    underline,
                    width,
                    gap: *gap,
                });
                *gap = false;
            }
        };
        for c in run.text.chars() {
            if c.is_whitespace() {
                flush(&mut word, &mut pending_gap, &mut tokens);
                pending_gap = true;
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut pending_gap, &mut tokens);
    }
    if let Some(first) = tokens.first_mut() {
        first.gap = false;
    }
    tokens
}

fn space_width(t: &Token) -> f32 {
    char_width(' ', t.font) * t.size
}

/// Break tokens into lines no wider than `width`, splitting over-long words.
fn wrap(tokens: Vec<Token>, width: f32) -> Vec<Vec<Token>> {
    let mut lines: Vec<Vec<Token>> = Vec::new();
    let mut line: Vec<Token> = Vec::new();
    let mut used = 0.0f32;
    let mut queue: std::collections::VecDeque<Token> = tokens.into();
    while let Some(mut token) = queue.pop_front() {
        if token.width > width {
            // Split by characters so the remainder keeps flowing.
            let mut head = String::new();
            let mut w = 0.0;
            let mut rest = String::new();
            for c in token.text.chars() {
                let cw = char_width(c, token.font) * token.size;
                if rest.is_empty() && (w + cw <= width || head.is_empty()) {
                    head.push(c);
                    w += cw;
                } else {
                    rest.push(c);
                }
            }
            if !rest.is_empty() {
                let tail = Token {
                    width: token_width(&rest, token.font, token.size),
                    text: rest,
                    gap: false,
                    ..token.clone()
                };
                queue.push_front(tail);
                token.text = head;
                token.width = w;
                if !line.is_empty() {
                    token.gap = false;
                    lines.push(std::mem::take(&mut line));
                    used = 0.0;
                }
            }
        }
        let gap = if token.gap && !line.is_empty() {
            space_width(&token)
        } else {
            0.0
        };
        if !line.is_empty() && used + gap + token.width > width + 0.01 {
            lines.push(std::mem::take(&mut line));
            used = 0.0;
            token.gap = false;
        } else if line.is_empty() {
            token.gap = false;
        }
        used += if token.gap { space_width(&token) } else { 0.0 } + token.width;
        line.push(token);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn line_height(line: &[Token]) -> f32 {
    line.iter().map(|t| t.size).fold(0.0, f32::max) * 1.3
}

fn natural_width(line: &[Token]) -> f32 {
    line.iter()
        .map(|t| t.width + if t.gap { space_width(t) } else { 0.0 })
        .sum()
}

fn num(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

struct Layout {
    pages: Vec<String>,
    cur: String,
    y: f32,
    left: f32,
    width: f32,
    top: f32,
    bottom: f32,
    page_h: f32,
}

impl Layout {
    fn new(size: PageSize, margin: f32) -> Layout {
        let (w, h) = size.points();
        Layout {
            pages: Vec::new(),
            cur: String::new(),
            y: h - margin,
            left: margin,
            width: w - 2.0 * margin,
            top: h - margin,
            bottom: margin,
            page_h: h,
        }
    }

    fn new_page(&mut self) -> Result<()> {
        if self.pages.len() >= MAX_PDF_PAGES {
            return invalid(format!("the document would exceed {MAX_PDF_PAGES} pages"));
        }
        self.pages.push(std::mem::take(&mut self.cur));
        self.y = self.top;
        Ok(())
    }

    fn ensure(&mut self, height: f32) -> Result<()> {
        if self.y - height < self.bottom && self.y < self.top {
            self.new_page()?;
        }
        Ok(())
    }

    fn draw_line(
        &mut self,
        line: &[Token],
        x: f32,
        baseline: f32,
        width: f32,
        align: Align,
        last: bool,
    ) {
        let natural = natural_width(line);
        let gaps = line.iter().filter(|t| t.gap).count();
        let (mut cursor, extra) = match align {
            Align::Left => (x, 0.0),
            Align::Center => (x + (width - natural).max(0.0) / 2.0, 0.0),
            Align::Right => (x + (width - natural).max(0.0), 0.0),
            Align::Justify if !last && gaps > 0 => (x, (width - natural).max(0.0) / gaps as f32),
            Align::Justify => (x, 0.0),
        };
        // Adjacent tokens sharing a look are emitted as one string so the
        // text extracts as running prose.
        let mut i = 0;
        while i < line.len() {
            let first = &line[i];
            if first.gap {
                cursor += space_width(first) + extra;
            }
            let mut text = first.text.clone();
            let mut seg = first.width;
            let mut j = i + 1;
            let mut spaces = 0usize;
            while j < line.len() {
                let t = &line[j];
                let same = t.font == first.font
                    && t.size == first.size
                    && t.color == first.color
                    && t.underline == first.underline;
                if !same || !t.gap {
                    break;
                }
                text.push(' ');
                text.push_str(&t.text);
                seg += space_width(t) + t.width;
                spaces += 1;
                j += 1;
            }
            let seg = seg + extra * spaces as f32;
            let (r, g, b) = first.color;
            self.cur.push_str(&format!(
                "BT /F{} {} Tf {} {} {} rg {} Tw 1 0 0 1 {} {} Tm {} Tj ET\n",
                first.font,
                num(first.size),
                num(r),
                num(g),
                num(b),
                num(extra),
                num(cursor),
                num(baseline),
                literal(&text)
            ));
            if first.underline {
                let uy = baseline - first.size * 0.12;
                self.cur.push_str(&format!(
                    "{} {} {} RG 0.6 w {} {} m {} {} l S\n",
                    num(r),
                    num(g),
                    num(b),
                    num(cursor),
                    num(uy),
                    num(cursor + seg),
                    num(uy)
                ));
            }
            cursor += seg;
            i = j;
        }
    }

    fn paragraph(
        &mut self,
        tokens: Vec<Token>,
        indent: f32,
        align: Align,
        marker: Option<Token>,
        after: f32,
    ) -> Result<()> {
        let lines = wrap(tokens, self.width - indent);
        let count = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let lh = line_height(line);
            self.ensure(lh)?;
            self.y -= lh;
            let baseline = self.y + lh * 0.25;
            if i == 0 {
                if let Some(m) = &marker {
                    let mx = self.left + indent - 4.0 - m.width;
                    self.draw_line(
                        std::slice::from_ref(m),
                        mx.max(self.left),
                        baseline,
                        m.width,
                        Align::Left,
                        true,
                    );
                }
            }
            let (x, w) = (self.left + indent, self.width - indent);
            self.draw_line(line, x, baseline, w, align, i + 1 == count);
        }
        self.y -= after;
        Ok(())
    }

    fn table(&mut self, rows: &[Vec<String>], header: bool, base: &Style) -> Result<()> {
        let cols = rows.first().map_or(1, Vec::len).max(1);
        let col_w = self.width / cols as f32;
        let pad = 4.0;
        for (r, row) in rows.iter().enumerate() {
            let head = header && r == 0;
            let style = Style {
                bold: if head { Some(true) } else { base.bold },
                ..base.clone()
            };
            let cells: Vec<Vec<Vec<Token>>> = row
                .iter()
                .map(|cell| {
                    wrap(
                        tokens_of(&[Run::plain(cell.clone())], &style),
                        col_w - 2.0 * pad,
                    )
                })
                .collect();
            let height = cells
                .iter()
                .map(|lines| lines.iter().map(|l| line_height(l)).sum::<f32>())
                .fold(0.0, f32::max)
                .max(14.0)
                + 2.0 * pad;
            self.ensure(height)?;
            let top = self.y;
            for (c, lines) in cells.iter().enumerate() {
                let x = self.left + c as f32 * col_w;
                if head {
                    self.cur.push_str(&format!(
                        "0.9 g {} {} {} {} re f\n",
                        num(x),
                        num(top - height),
                        num(col_w),
                        num(height)
                    ));
                }
                self.cur.push_str(&format!(
                    "0.5 G 0.5 w {} {} {} {} re S\n",
                    num(x),
                    num(top - height),
                    num(col_w),
                    num(height)
                ));
                let mut y = top - pad;
                for line in lines {
                    let lh = line_height(line);
                    y -= lh;
                    self.draw_line(
                        line,
                        x + pad,
                        y + lh * 0.25,
                        col_w - 2.0 * pad,
                        Align::Left,
                        true,
                    );
                }
            }
            self.y = top - height;
        }
        self.y -= 8.0;
        Ok(())
    }
}

fn render(spec: &Spec) -> Result<Vec<String>> {
    let mut l = Layout::new(spec.page_size, spec.margin);
    let mut base = spec.base.clone();
    base.size.get_or_insert(11.0);
    if let Some(title) = &spec.title {
        let style = Style {
            size: Some(26.0),
            bold: Some(true),
            ..base.clone()
        };
        let tokens = tokens_of(&[Run::plain(title.clone())], &style);
        l.paragraph(tokens, 0.0, Align::Left, None, 10.0)?;
    }
    for block in &spec.blocks {
        match block {
            Block::Heading { level, runs, style } => {
                let size = match level {
                    1 => 24.0,
                    2 => 18.0,
                    _ => 14.0,
                };
                let heading = Style {
                    size: Some(size),
                    bold: Some(true),
                    ..base.clone()
                };
                let merged = style.over(&heading);
                // Keep a heading with the paragraph that follows it.
                l.ensure(size * 1.3 * 3.0)?;
                l.y -= 6.0;
                let tokens = tokens_of(runs, &merged);
                l.paragraph(tokens, 0.0, merged.align.unwrap_or(Align::Left), None, 4.0)?;
            }
            Block::Paragraph { runs, style } => {
                let merged = style.over(&base);
                let tokens = tokens_of(runs, &merged);
                l.paragraph(tokens, 0.0, merged.align.unwrap_or(Align::Left), None, 6.0)?;
            }
            Block::List {
                ordered,
                items,
                style,
            } => {
                let merged = style.over(&base);
                for (i, item) in items.iter().enumerate() {
                    let label = if *ordered {
                        format!("{}.", i + 1)
                    } else {
                        "•".to_string()
                    };
                    let marker = tokens_of(&[Run::plain(label)], &merged).pop();
                    let tokens = tokens_of(&[Run::plain(item.clone())], &merged);
                    l.paragraph(
                        tokens,
                        22.0,
                        merged.align.unwrap_or(Align::Left),
                        marker,
                        2.0,
                    )?;
                }
                l.y -= 4.0;
            }
            Block::Table { rows, header } => l.table(rows, *header, &base)?,
            Block::PageBreak => l.new_page()?,
        }
    }
    l.pages.push(std::mem::take(&mut l.cur));
    let _ = l.page_h;
    if spec.page_numbers {
        let total = l.pages.len();
        let (w, _) = spec.page_size.points();
        for (i, page) in l.pages.iter_mut().enumerate() {
            let label = format!("{} / {}", i + 1, total);
            let width = token_width(&label, 0, 9.0);
            page.push_str(&format!(
                "BT /F0 9 Tf 0 g 1 0 0 1 {} {} Tm {} Tj ET\n",
                num((w - width) / 2.0),
                num((spec.margin / 2.0 - 4.0).max(8.0)),
                literal(&label)
            ));
        }
    }
    Ok(l.pages)
}

fn pdf_text(text: &str) -> Object {
    let bytes: Vec<u8> = text.chars().map(to_win_ansi).collect();
    Object::string_literal(bytes)
}

fn fail(e: impl std::fmt::Display) -> DocError {
    DocError::Invalid(format!("the PDF could not be processed: {e}"))
}

pub fn create(spec: &Spec) -> Result<Vec<u8>> {
    let pages = render(spec)?;
    let (w, h) = spec.page_size.points();
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut fonts = Dictionary::new();
    for (i, name) in FONT_NAMES.iter().enumerate() {
        let id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => *name,
            "Encoding" => "WinAnsiEncoding",
        });
        fonts.set(format!("F{i}"), id);
    }
    let resources = dictionary! { "Font" => fonts };
    let mut kids = Vec::new();
    for content in pages {
        let mut stream = Stream::new(Dictionary::new(), content.into_bytes());
        let _ = stream.compress();
        let content_id = doc.add_object(stream);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)],
            "Resources" => resources.clone(),
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut info = Dictionary::new();
    if let Some(t) = &spec.title {
        info.set("Title", pdf_text(t));
    }
    if let Some(a) = &spec.author {
        info.set("Author", pdf_text(a));
    }
    info.set("Producer", pdf_text("Lia"));
    let info_id = doc.add_object(info);
    doc.trailer.set("Info", info_id);
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| DocError::Io(e.to_string()))?;
    Ok(out)
}

fn load(bytes: &[u8]) -> Result<Document> {
    let mut doc = Document::load_mem(bytes).map_err(fail)?;
    if doc.is_encrypted() && doc.decrypt("").is_err() {
        return Err(DocError::Unsupported(
            "the PDF is password protected".to_string(),
        ));
    }
    Ok(doc)
}

fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xfe, 0xff]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

fn info_field(doc: &Document, key: &[u8]) -> Option<String> {
    let id = doc.trailer.get(b"Info").ok()?.as_reference().ok()?;
    let dict = doc.get_dictionary(id).ok()?;
    let bytes = dict.get(key).ok()?.as_str().ok()?;
    let text = decode_text(bytes);
    (!text.trim().is_empty()).then_some(text)
}

/// Extract the text of a PDF, one `--- page n ---` section per page.
pub fn read(bytes: &[u8], pages: Option<&[u32]>, max_chars: usize) -> Result<String> {
    let doc = load(bytes)?;
    let all = doc.get_pages();
    let total = all.len();
    let wanted: Vec<u32> = match pages {
        Some(list) => {
            if let Some(bad) = list.iter().find(|p| !all.contains_key(p)) {
                return invalid(format!(
                    "page {bad} does not exist; the PDF has {total} pages"
                ));
            }
            list.to_vec()
        }
        None => all.keys().copied().collect(),
    };
    let mut out = format!("PDF, {total} page(s)");
    if let Some(t) = info_field(&doc, b"Title") {
        out.push_str(&format!(", title: {t}"));
    }
    if let Some(a) = info_field(&doc, b"Author") {
        out.push_str(&format!(", author: {a}"));
    }
    out.push('\n');
    let mut empty = 0usize;
    for page in &wanted {
        let text = doc.extract_text(&[*page]).unwrap_or_default();
        let text = text.trim();
        if text.is_empty() {
            empty += 1;
        }
        out.push_str(&format!("\n--- page {page} ---\n{text}\n"));
        if out.chars().count() > max_chars {
            let cut: String = out.chars().take(max_chars).collect();
            return Ok(format!("{cut}\n[truncated at {max_chars} characters]"));
        }
    }
    if empty == wanted.len() && !wanted.is_empty() {
        out.push_str("\n[no text could be extracted; the PDF may be scanned images]\n");
    }
    Ok(out)
}

fn pages_root(doc: &Document) -> Result<ObjectId> {
    doc.catalog()
        .map_err(fail)?
        .get(b"Pages")
        .and_then(Object::as_reference)
        .map_err(fail)
}

fn append(doc: &mut Document, op: &Value) -> Result<usize> {
    let mut spec_value = op.clone();
    if let Some(map) = spec_value.as_object_mut() {
        map.remove("op");
    }
    let spec = Spec::parse(&spec_value)?;
    let mut added = load(&create(&spec)?)?;
    let root = pages_root(doc)?;
    added.renumber_objects_with(doc.max_id + 1);
    doc.max_id = added.max_id;
    let new_pages: Vec<ObjectId> = added.get_pages().values().copied().collect();
    for (id, mut object) in std::mem::take(&mut added.objects) {
        if new_pages.contains(&id) {
            if let Object::Dictionary(d) = &mut object {
                d.set("Parent", root);
            }
        }
        doc.objects.insert(id, object);
    }
    let dict = doc.get_dictionary_mut(root).map_err(fail)?;
    let mut kids = dict
        .get(b"Kids")
        .and_then(Object::as_array)
        .map_err(fail)?
        .clone();
    kids.extend(new_pages.iter().map(|id| Object::Reference(*id)));
    let count = kids.len() as i64;
    dict.set("Kids", kids);
    dict.set("Count", count);
    Ok(new_pages.len())
}

fn page_list(op: &Value) -> Result<Vec<u32>> {
    let list = op
        .get("pages")
        .and_then(Value::as_array)
        .ok_or_else(|| DocError::Invalid("delete_pages needs a pages array".to_string()))?;
    list.iter()
        .map(|v| {
            v.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n >= 1)
                .ok_or_else(|| DocError::Invalid("pages are 1-based integers".to_string()))
        })
        .collect()
}

/// Apply edit operations; returns the new file and a note per operation.
pub fn edit(bytes: &[u8], ops: &Value) -> Result<(Vec<u8>, Vec<String>)> {
    let ops = ops
        .as_array()
        .filter(|o| !o.is_empty())
        .ok_or_else(|| DocError::Invalid("ops must be a non-empty array".to_string()))?;
    let mut doc = load(bytes)?;
    let mut notes = Vec::new();
    for op in ops {
        let name = op.get("op").and_then(Value::as_str).unwrap_or("");
        match name {
            "delete_pages" => {
                let mut pages = page_list(op)?;
                pages.sort_unstable();
                pages.dedup();
                let total = doc.get_pages().len();
                if let Some(bad) = pages.iter().find(|p| **p as usize > total) {
                    return invalid(format!(
                        "page {bad} does not exist; the PDF has {total} pages"
                    ));
                }
                if pages.len() >= total {
                    return invalid("a PDF must keep at least one page");
                }
                doc.delete_pages(&pages);
                notes.push(format!("deleted {} page(s)", pages.len()));
            }
            "replace_text" => {
                let find = op.get("find").and_then(Value::as_str).unwrap_or("");
                let with = op.get("replace").and_then(Value::as_str).unwrap_or("");
                if find.is_empty() {
                    return invalid("replace_text needs a non-empty find");
                }
                let only = op.get("page").and_then(Value::as_u64);
                let mut done = 0usize;
                for page in doc.get_pages().keys().copied().collect::<Vec<_>>() {
                    if only.is_some_and(|p| p != u64::from(page)) {
                        continue;
                    }
                    done += doc
                        .replace_partial_text(page, find, with, Some("?"))
                        .unwrap_or(0);
                }
                notes.push(if done == 0 {
                    format!("no editable text matched {find:?} (PDF text is often split or embedded as glyphs)")
                } else {
                    format!("replaced {done} occurrence(s) of {find:?}")
                });
            }
            "set_metadata" => {
                let id = match doc.trailer.get(b"Info").and_then(Object::as_reference) {
                    Ok(id) => id,
                    Err(_) => {
                        let id = doc.add_object(Dictionary::new());
                        doc.trailer.set("Info", id);
                        id
                    }
                };
                let dict = doc.get_dictionary_mut(id).map_err(fail)?;
                for (key, field) in [("Title", "title"), ("Author", "author")] {
                    if let Some(v) = op.get(field).and_then(Value::as_str) {
                        if v.len() > 500 {
                            return invalid("metadata values are limited to 500 bytes");
                        }
                        dict.set(key, pdf_text(v));
                    }
                }
                notes.push("updated metadata".to_string());
            }
            "append" => {
                let n = append(&mut doc, op)?;
                notes.push(format!("appended {n} page(s)"));
            }
            "style" | "insert" | "delete" => {
                return Err(DocError::Unsupported(format!(
                    "'{name}' cannot be applied to an existing PDF; create the document again as .docx or .pdf with the wanted styling"
                )));
            }
            other => {
                return invalid(format!(
                    "unknown PDF operation {other:?}; use delete_pages, replace_text, set_metadata or append"
                ));
            }
        }
    }
    doc.prune_objects();
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| DocError::Io(e.to_string()))?;
    Ok((out, notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(blocks: Value) -> Spec {
        Spec::parse(&json!({"title": "Report", "author": "Lia", "blocks": blocks})).unwrap()
    }

    fn sample() -> Vec<u8> {
        create(&spec(json!([
            {"type": "heading", "level": 1, "text": "Overview"},
            {"type": "paragraph", "text": "Hello (world) \\ budget"},
            {"type": "list", "items": ["alpha", "beta"]},
            {"type": "table", "rows": [["a", "b"], ["1", "2"]]},
        ])))
        .unwrap()
    }

    #[test]
    fn a_created_pdf_reads_back() {
        let text = read(&sample(), None, 10_000).unwrap();
        assert!(text.contains("title: Report"), "{text}");
        assert!(text.contains("Overview"), "{text}");
        assert!(text.contains("Hello"), "{text}");
        assert!(text.contains("alpha"), "{text}");
    }

    #[test]
    fn long_documents_paginate_and_pages_can_be_selected() {
        let blocks: Vec<Value> = (0..200)
            .map(|i| json!({"type": "paragraph", "text": format!("line number {i} of the report")}))
            .collect();
        let bytes = create(&spec(Value::Array(blocks))).unwrap();
        let all = read(&bytes, None, 1_000_000).unwrap();
        assert!(all.contains("--- page 2 ---"));
        let second = read(&bytes, Some(&[2]), 1_000_000).unwrap();
        assert!(second.contains("--- page 2 ---") && !second.contains("--- page 1 ---"));
        assert!(read(&bytes, Some(&[99]), 100).is_err());
    }

    #[test]
    fn page_numbers_and_page_breaks_work() {
        let s = Spec::parse(&json!({
            "page_numbers": true,
            "blocks": [{"type": "paragraph", "text": "one"}, {"type": "page_break"},
                       {"type": "paragraph", "text": "two"}]
        }))
        .unwrap();
        let text = read(&create(&s).unwrap(), None, 10_000).unwrap();
        assert!(text.contains("2 page(s)"), "{text}");
        assert!(text.contains("1 / 2") && text.contains("2 / 2"), "{text}");
    }

    #[test]
    fn edits_delete_append_and_retitle() {
        let (bytes, notes) = edit(
            &sample(),
            &json!([
                {"op": "append", "blocks": [{"type": "paragraph", "text": "Appendix text"}]},
                {"op": "set_metadata", "title": "Final"},
            ]),
        )
        .unwrap();
        assert_eq!(notes.len(), 2);
        let text = read(&bytes, None, 10_000).unwrap();
        assert!(
            text.contains("2 page(s)") && text.contains("Appendix text"),
            "{text}"
        );
        assert!(text.contains("title: Final"), "{text}");
        let (bytes, _) = edit(&bytes, &json!([{"op": "delete_pages", "pages": [1]}])).unwrap();
        let text = read(&bytes, None, 10_000).unwrap();
        assert!(
            text.contains("1 page(s)") && !text.contains("Overview"),
            "{text}"
        );
    }

    #[test]
    fn replacement_is_reported_and_style_edits_are_refused() {
        let (_, notes) = edit(
            &sample(),
            &json!([{"op": "replace_text", "find": "Overview", "replace": "Summary"}]),
        )
        .unwrap();
        assert!(notes[0].contains("replaced") || notes[0].contains("no editable"));
        assert!(matches!(
            edit(
                &sample(),
                &json!([{"op": "style", "find": "x", "style": {}}])
            ),
            Err(DocError::Unsupported(_))
        ));
        assert!(edit(&sample(), &json!([{"op": "delete_pages", "pages": [1]}])).is_err());
        assert!(read(b"not a pdf", None, 10).is_err());
    }

    #[test]
    fn long_words_and_odd_characters_do_not_break_layout() {
        let long = "x".repeat(400);
        let bytes = create(&spec(json!([
            {"type": "paragraph", "text": long},
            {"type": "paragraph", "text": "caf\u{e9} \u{20ac} \u{4e2d}", "align": "justify"},
        ])))
        .unwrap();
        assert!(read(&bytes, None, 10_000).is_ok());
    }
}
