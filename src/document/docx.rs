//! DOCX: create from a spec, read as text, and edit in place.
//!
//! Creating writes a small, valid package (document, styles, numbering,
//! optional page-number footer, core properties). Editing parses only
//! `word/document.xml` and changes just the paragraphs an operation targets;
//! every other part of the package is copied through unchanged.

use std::io::{Cursor, Read, Write};

use serde::Deserialize;
use serde_json::Value;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::xml::{self, Element, Node};
use super::{Align, Block, DocError, Result, Run, Spec, Style, invalid};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

fn io(e: impl std::fmt::Display) -> DocError {
    DocError::Io(e.to_string())
}

fn zip_err(e: impl std::fmt::Display) -> DocError {
    DocError::Invalid(format!("not a valid .docx file: {e}"))
}

fn twips(points: f32) -> u32 {
    (points * 20.0).round() as u32
}

// ---------------------------------------------------------------- rendering

struct Ctx {
    /// Text width in twips, for table columns.
    text_width: u32,
    /// `Some` when numbering.xml is part of the package being written.
    ordered_lists: Option<u32>,
    /// Format headings directly, for documents whose styles are unknown.
    direct_headings: bool,
}

fn rpr(style: &Style) -> String {
    let mut s = String::new();
    if let Some(font) = &style.font {
        let f = xml::escape(font);
        s.push_str(&format!(
            "<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:cs=\"{f}\" w:eastAsia=\"{f}\"/>"
        ));
    }
    match style.bold {
        Some(true) => s.push_str("<w:b/><w:bCs/>"),
        Some(false) => s.push_str("<w:b w:val=\"0\"/><w:bCs w:val=\"0\"/>"),
        None => {}
    }
    match style.italic {
        Some(true) => s.push_str("<w:i/><w:iCs/>"),
        Some(false) => s.push_str("<w:i w:val=\"0\"/><w:iCs w:val=\"0\"/>"),
        None => {}
    }
    if let Some(color) = &style.color {
        s.push_str(&format!("<w:color w:val=\"{color}\"/>"));
    }
    if let Some(size) = style.size {
        let half = (size * 2.0).round() as u32;
        s.push_str(&format!(
            "<w:sz w:val=\"{half}\"/><w:szCs w:val=\"{half}\"/>"
        ));
    }
    match style.underline {
        Some(true) => s.push_str("<w:u w:val=\"single\"/>"),
        Some(false) => s.push_str("<w:u w:val=\"none\"/>"),
        None => {}
    }
    if s.is_empty() {
        s
    } else {
        format!("<w:rPr>{s}</w:rPr>")
    }
}

fn jc(align: Align) -> &'static str {
    match align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "both",
    }
}

fn run_xml(run: &Run, extra: &Style) -> String {
    let style = run.style.over(extra);
    let mut out = format!("<w:r>{}", rpr(&style));
    let mut part = String::new();
    let flush = |out: &mut String, part: &mut String| {
        if !part.is_empty() {
            out.push_str("<w:t xml:space=\"preserve\">");
            xml::escape_into(out, part, false);
            out.push_str("</w:t>");
            part.clear();
        }
    };
    for c in run.text.chars() {
        match c {
            '\n' => {
                flush(&mut out, &mut part);
                out.push_str("<w:br/>");
            }
            '\t' => {
                flush(&mut out, &mut part);
                out.push_str("<w:tab/>");
            }
            c => part.push(c),
        }
    }
    flush(&mut out, &mut part);
    out.push_str("</w:r>");
    out
}

fn para(
    style_id: Option<&str>,
    pre_ppr: &str,
    align: Option<Align>,
    runs: &[Run],
    shared: &Style,
) -> String {
    let mut ppr = String::new();
    if let Some(id) = style_id {
        ppr.push_str(&format!("<w:pStyle w:val=\"{id}\"/>"));
    }
    ppr.push_str(pre_ppr);
    if let Some(a) = align {
        ppr.push_str(&format!("<w:jc w:val=\"{}\"/>", jc(a)));
    }
    let ppr = if ppr.is_empty() {
        ppr
    } else {
        format!("<w:pPr>{ppr}</w:pPr>")
    };
    let body: String = runs.iter().map(|r| run_xml(r, shared)).collect();
    format!("<w:p>{ppr}{body}</w:p>")
}

/// Run-level look from a paragraph-level style (alignment excluded).
fn run_level(style: &Style) -> Style {
    Style {
        align: None,
        ..style.clone()
    }
}

fn render_blocks(blocks: &[Block], ctx: &mut Ctx) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            Block::Heading { level, runs, style } => {
                let mut shared = run_level(style);
                let id = format!("Heading{level}");
                let mut pre = String::new();
                if ctx.direct_headings {
                    let size = [16.0, 13.0, 12.0][*level as usize - 1];
                    shared = shared.over(&Style {
                        size: Some(size),
                        bold: Some(true),
                        color: Some("1F3864".into()),
                        ..Style::default()
                    });
                    pre = format!(
                        "<w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"{}\"/>",
                        level - 1
                    );
                }
                out.push_str(&para(Some(&id), &pre, style.align, runs, &shared));
            }
            Block::Paragraph { runs, style } => {
                out.push_str(&para(None, "", style.align, runs, &run_level(style)));
            }
            Block::List {
                ordered,
                items,
                style,
            } => {
                let numbering = ctx.ordered_lists.is_some();
                let num_id = if *ordered {
                    let next = ctx.ordered_lists.as_mut().map(|n| {
                        *n += 1;
                        *n
                    });
                    next.unwrap_or(0)
                } else {
                    1
                };
                for (index, item) in items.iter().enumerate() {
                    let runs = if numbering {
                        vec![Run::plain(item.clone())]
                    } else {
                        let marker = if *ordered {
                            format!("{}.\t", index + 1)
                        } else {
                            "\u{2022}\t".to_owned()
                        };
                        vec![Run::plain(format!("{marker}{item}"))]
                    };
                    let pre = if numbering {
                        format!(
                            "<w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"{num_id}\"/></w:numPr>"
                        )
                    } else {
                        "<w:ind w:left=\"720\" w:hanging=\"360\"/>".to_owned()
                    };
                    let id = numbering.then_some("ListParagraph");
                    out.push_str(&para(id, &pre, style.align, &runs, &run_level(style)));
                }
            }
            Block::Table { rows, header } => {
                let cols = rows[0].len() as u32;
                let width = ctx.text_width.max(cols * 360);
                let col = width / cols;
                out.push_str(&format!(
                    "<w:tbl><w:tblPr><w:tblW w:w=\"{width}\" w:type=\"dxa\"/><w:tblBorders>"
                ));
                for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                    out.push_str(&format!(
                        "<w:{side} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>"
                    ));
                }
                out.push_str("</w:tblBorders><w:tblLayout w:type=\"fixed\"/></w:tblPr><w:tblGrid>");
                for _ in 0..cols {
                    out.push_str(&format!("<w:gridCol w:w=\"{col}\"/>"));
                }
                out.push_str("</w:tblGrid>");
                for (r, row) in rows.iter().enumerate() {
                    let head = *header && r == 0;
                    out.push_str("<w:tr>");
                    if head {
                        out.push_str("<w:trPr><w:tblHeader/></w:trPr>");
                    }
                    for cell in row {
                        let shade = if head {
                            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"D9E2F3\"/>"
                        } else {
                            ""
                        };
                        let style = Style {
                            bold: head.then_some(true),
                            ..Style::default()
                        };
                        out.push_str(&format!(
                            "<w:tc><w:tcPr><w:tcW w:w=\"{col}\" w:type=\"dxa\"/>{shade}</w:tcPr>{}</w:tc>",
                            para(None, "", None, &[Run::plain(cell.clone())], &style)
                        ));
                    }
                    out.push_str("</w:tr>");
                }
                out.push_str("</w:tbl><w:p/>");
            }
            Block::PageBreak => {
                out.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
            }
        }
    }
    out
}

// ------------------------------------------------------------------- create

/// Render a spec as a `.docx` package.
pub fn create(spec: &Spec) -> Result<Vec<u8>> {
    let (page_w, page_h) = spec.page_size.points();
    let margin = twips(spec.margin);
    let text_width = twips(page_w) - 2 * margin;
    let mut ctx = Ctx {
        text_width,
        ordered_lists: Some(1),
        direct_headings: false,
    };

    let mut body = String::new();
    if let Some(title) = &spec.title {
        body.push_str(&para(
            Some("Title"),
            "",
            None,
            &[Run::plain(title.clone())],
            &Style::default(),
        ));
    }
    body.push_str(&render_blocks(&spec.blocks, &mut ctx));
    let ordered = ctx.ordered_lists.unwrap_or(1) - 1;

    let footer_ref = if spec.page_numbers {
        "<w:footerReference w:type=\"default\" r:id=\"rIdFooter\"/>"
    } else {
        ""
    };
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\"><w:body>{body}<w:sectPr>{footer_ref}<w:pgSz w:w=\"{}\" w:h=\"{}\"/><w:pgMar w:top=\"{margin}\" w:right=\"{margin}\" w:bottom=\"{margin}\" w:left=\"{margin}\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>",
        twips(page_w),
        twips(page_h)
    );

    let mut content_types = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
    );
    if spec.page_numbers {
        content_types.push_str("<Override PartName=\"/word/footer1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml\"/>");
    }
    content_types.push_str("</Types>");

    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";

    let mut doc_rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/><Relationship Id=\"rIdNumbering\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>",
    );
    if spec.page_numbers {
        doc_rels.push_str("<Relationship Id=\"rIdFooter\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer\" Target=\"footer1.xml\"/>");
    }
    doc_rels.push_str("</Relationships>");

    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title><dc:creator>{}</dc:creator></cp:coreProperties>",
        xml::escape(spec.title.as_deref().unwrap_or("")),
        xml::escape(spec.author.as_deref().unwrap_or(""))
    );

    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut buffer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut put = |name: &str, data: &str| -> Result<()> {
            zip.start_file(name, options).map_err(io)?;
            zip.write_all(data.as_bytes()).map_err(io)
        };
        put("[Content_Types].xml", &content_types)?;
        put("_rels/.rels", rels)?;
        put("word/document.xml", &document)?;
        put("word/_rels/document.xml.rels", &doc_rels)?;
        put("word/styles.xml", &styles_xml(&spec.base))?;
        put("word/numbering.xml", &numbering_xml(ordered))?;
        put("docProps/core.xml", &core)?;
        if spec.page_numbers {
            put("word/footer1.xml", &footer_xml())?;
        }
        zip.finish().map_err(io)?;
    }
    Ok(buffer.into_inner())
}

fn styles_xml(base: &Style) -> String {
    let font = xml::escape(base.font.as_deref().unwrap_or("Calibri"));
    let size = (base.size.unwrap_or(11.0) * 2.0).round() as u32;
    let color = base
        .color
        .as_deref()
        .map(|c| format!("<w:color w:val=\"{c}\"/>"))
        .unwrap_or_default();
    let jc = base
        .align
        .map(|a| format!("<w:jc w:val=\"{}\"/>", jc(a)))
        .unwrap_or_default();
    let heading = |level: u32, size: u32| {
        format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{level}\"><w:name w:val=\"heading {level}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/></w:rPr></w:style>",
            level - 1
        )
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"{W_NS}\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\" w:cs=\"{font}\" w:eastAsia=\"{font}\"/>{color}<w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/>{jc}</w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style><w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:spacing w:after=\"240\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:color w:val=\"1F3864\"/><w:sz w:val=\"56\"/><w:szCs w:val=\"56\"/></w:rPr></w:style>{}{}{}<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/><w:pPr><w:spacing w:after=\"60\"/><w:ind w:left=\"720\"/></w:pPr></w:style></w:styles>",
        heading(1, 32),
        heading(2, 28),
        heading(3, 24),
    )
}

fn numbering_xml(ordered_lists: u32) -> String {
    let level = |fmt: &str, text: &str| {
        format!(
            "<w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"720\" w:hanging=\"360\"/></w:pPr></w:lvl>"
        )
    };
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering xmlns:w=\"{W_NS}\"><w:abstractNum w:abstractNumId=\"0\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{}</w:abstractNum><w:abstractNum w:abstractNumId=\"1\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{}</w:abstractNum><w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>",
        level("bullet", "\u{2022}"),
        level("decimal", "%1.")
    );
    // Each ordered list gets its own instance so numbering restarts at 1.
    for n in 0..ordered_lists {
        s.push_str(&format!(
            "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"1\"/><w:lvlOverride w:ilvl=\"0\"><w:startOverride w:val=\"1\"/></w:lvlOverride></w:num>",
            n + 2
        ));
    }
    s.push_str("</w:numbering>");
    s
}

fn footer_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:ftr xmlns:w=\"{W_NS}\"><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p></w:ftr>"
    )
}

// -------------------------------------------------------------------- parts

fn read_part(bytes: &[u8], name: &str) -> Result<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;
    if archive.len() > MAX_ENTRIES {
        return invalid("the file has too many parts");
    }
    let file = archive
        .by_name(name)
        .map_err(|_| DocError::Invalid(format!("not a .docx file: {name} is missing")))?;
    let mut data = Vec::new();
    file.take(MAX_PART_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(io)?;
    if data.len() as u64 > MAX_PART_BYTES {
        return invalid("the document body is too large");
    }
    Ok(data)
}

fn load_document(bytes: &[u8]) -> Result<Element> {
    let root = xml::parse(&read_part(bytes, "word/document.xml")?)?;
    if root.name != "w:document" || root.child("w:body").is_none() {
        return invalid("not a Word document: no w:body");
    }
    Ok(root)
}

/// Copy the package, replacing `word/document.xml`.
fn rebuild(original: &[u8], document: &Element) -> Result<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(original)).map_err(zip_err)?;
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut buffer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for index in 0..archive.len() {
            let file = archive.by_index_raw(index).map_err(zip_err)?;
            if file.name().ok().as_deref() == Some("word/document.xml") {
                zip.start_file("word/document.xml", options).map_err(io)?;
                zip.write_all(xml::to_document(document).as_bytes())
                    .map_err(io)?;
            } else {
                zip.raw_copy_file(file).map_err(io)?;
            }
        }
        zip.finish().map_err(io)?;
    }
    Ok(buffer.into_inner())
}

// ---------------------------------------------------------------- traversal

const CONTAINERS: [&str; 7] = [
    "w:body",
    "w:tbl",
    "w:tr",
    "w:tc",
    "w:sdt",
    "w:sdtContent",
    "w:customXml",
];

/// Where a paragraph sits.
#[derive(Debug, Clone)]
struct ParaRef {
    path: Vec<usize>,
    table: Option<(usize, usize, usize)>,
}

fn paragraphs(root: &Element) -> Vec<ParaRef> {
    fn walk(
        element: &Element,
        path: &mut Vec<usize>,
        table: Option<(usize, usize, usize)>,
        tables: &mut usize,
        out: &mut Vec<ParaRef>,
    ) {
        let mut row = 0usize;
        let mut cell = 0usize;
        for (index, node) in element.children.iter().enumerate() {
            let Node::El(child) = node else { continue };
            path.push(index);
            if child.name == "w:p" {
                out.push(ParaRef {
                    path: path.clone(),
                    table,
                });
            } else if child.name == "w:tbl" {
                *tables += 1;
                let id = *tables;
                walk(child, path, Some((id, 0, 0)), tables, out);
            } else if CONTAINERS.contains(&child.name.as_str()) {
                let inner = match (child.name.as_str(), table) {
                    ("w:tr", Some((t, _, _))) => {
                        row += 1;
                        cell = 0;
                        Some((t, row, 0))
                    }
                    ("w:tc", Some((t, r, _))) => {
                        cell += 1;
                        Some((t, r, cell))
                    }
                    _ => table,
                };
                walk(child, path, inner, tables, out);
            }
            path.pop();
        }
    }
    let Some(body) = root.child("w:body") else {
        return Vec::new();
    };
    let body_index = root
        .children
        .iter()
        .position(|n| matches!(n, Node::El(e) if e.name == "w:body"))
        .unwrap_or(0);
    let mut out = Vec::new();
    let mut path = vec![body_index];
    let mut tables = 0;
    walk(body, &mut path, None, &mut tables, &mut out);
    out
}

fn at<'a>(root: &'a Element, path: &[usize]) -> &'a Element {
    let mut e = root;
    for &i in path {
        match &e.children[i] {
            Node::El(c) => e = c,
            Node::Text(_) => unreachable!("paths name elements"),
        }
    }
    e
}

fn at_mut<'a>(root: &'a mut Element, path: &[usize]) -> &'a mut Element {
    let mut e = root;
    for &i in path {
        match &mut e.children[i] {
            Node::El(c) => e = c,
            Node::Text(_) => unreachable!("paths name elements"),
        }
    }
    e
}

/// Paths (relative to `p`) of text-bearing elements, in order, skipping
/// deleted text, text boxes and alternate-content fallbacks.
fn text_paths(p: &Element) -> Vec<(Vec<usize>, &'static str)> {
    fn walk(e: &Element, path: &mut Vec<usize>, out: &mut Vec<(Vec<usize>, &'static str)>) {
        for (index, node) in e.children.iter().enumerate() {
            let Node::El(child) = node else { continue };
            path.push(index);
            match child.name.as_str() {
                "w:del" | "w:txbxContent" | "mc:Fallback" | "w:pPr" | "w:rPr" => {}
                "w:t" => out.push((path.clone(), "t")),
                "w:tab" => out.push((path.clone(), "tab")),
                "w:br" | "w:cr" => out.push((path.clone(), "br")),
                _ => walk(child, path, out),
            }
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(p, &mut Vec::new(), &mut out);
    out
}

fn paragraph_text(p: &Element) -> String {
    let mut s = String::new();
    for (path, kind) in text_paths(p) {
        match kind {
            "t" => s.push_str(&at(p, &path).own_text()),
            "tab" => s.push('\t'),
            _ => {
                if at(p, &path).attr("w:type") != Some("page") {
                    s.push('\n');
                }
            }
        }
    }
    s
}

fn heading_level(p: &Element) -> Option<usize> {
    let id = p.child("w:pPr")?.child("w:pStyle")?.attr("w:val")?;
    let lower = id.to_ascii_lowercase();
    if lower == "title" {
        return Some(1);
    }
    lower
        .strip_prefix("heading")
        .and_then(|n| n.trim().parse::<usize>().ok())
        .filter(|n| (1..=9).contains(n))
}

fn is_list_item(p: &Element) -> bool {
    p.child("w:pPr")
        .is_some_and(|ppr| ppr.child("w:numPr").is_some())
}

fn describe_format(p: &Element) -> String {
    let mut parts = Vec::new();
    if let Some(jc) = p
        .child("w:pPr")
        .and_then(|ppr| ppr.child("w:jc"))
        .and_then(|j| j.attr("w:val"))
    {
        parts.push(format!("align={jc}"));
    }
    let first_run = p.children.iter().find_map(|n| match n {
        Node::El(e) if e.name == "w:r" => Some(e),
        _ => None,
    });
    if let Some(rpr) = first_run.and_then(|r| r.child("w:rPr")) {
        if let Some(f) = rpr.child("w:rFonts").and_then(|f| f.attr("w:ascii")) {
            parts.push(format!("font={f}"));
        }
        if let Some(s) = rpr.child("w:sz").and_then(|s| s.attr("w:val"))
            && let Ok(half) = s.parse::<f32>()
        {
            parts.push(format!("size={}", half / 2.0));
        }
        let on = |name: &str| {
            rpr.child(name)
                .is_some_and(|e| !matches!(e.attr("w:val"), Some("0" | "false" | "none")))
        };
        for (tag, label) in [("w:b", "bold"), ("w:i", "italic"), ("w:u", "underline")] {
            if on(tag) {
                parts.push(label.to_owned());
            }
        }
        if let Some(c) = rpr.child("w:color").and_then(|c| c.attr("w:val")) {
            parts.push(format!("color={c}"));
        }
    }
    parts.join(" ")
}

// --------------------------------------------------------------------- read

/// Read a `.docx` as numbered text.
pub fn read(bytes: &[u8], max_chars: usize, formatting: bool) -> Result<String> {
    let root = load_document(bytes)?;
    let refs = paragraphs(&root);
    let tables = refs
        .iter()
        .filter_map(|r| r.table.map(|t| t.0))
        .max()
        .unwrap_or(0);
    let mut out = format!(
        "Word document: {} paragraphs, {} tables. Indexes in [n] are used by document_edit.\n",
        refs.len(),
        tables
    );
    for (n, r) in refs.iter().enumerate() {
        let p = at(&root, &r.path);
        let text = paragraph_text(p);
        if text.trim().is_empty() {
            continue;
        }
        let mut line = format!("[{}] ", n + 1);
        if let Some((t, row, cell)) = r.table {
            line.push_str(&format!("(table {t}, row {row}, cell {cell}) "));
        }
        if let Some(level) = heading_level(p) {
            line.push_str(&format!("{} ", "#".repeat(level)));
        } else if is_list_item(p) {
            line.push_str("- ");
        }
        line.push_str(&text);
        if formatting {
            let f = describe_format(p);
            if !f.is_empty() {
                line.push_str(&format!("  {{{f}}}"));
            }
        }
        line.push('\n');
        if out.len() + line.len() > max_chars {
            out.push_str("[output truncated; raise max_chars to read more]\n");
            break;
        }
        out.push_str(&line);
    }
    Ok(out)
}

// --------------------------------------------------------------------- edit

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Replace {
        find: String,
        replace: String,
        #[serde(default = "yes")]
        all: bool,
        #[serde(default = "yes")]
        match_case: bool,
        paragraph: Option<usize>,
    },
    Delete {
        paragraph: usize,
    },
    Insert {
        after: Option<usize>,
        blocks: Value,
    },
    Style {
        paragraph: Option<usize>,
        find: Option<String>,
        #[serde(default)]
        all: bool,
        style: Style,
    },
}

fn yes() -> bool {
    true
}

/// Apply edit operations. Returns the new package and a summary per operation.
pub fn edit(bytes: &[u8], ops: &Value) -> Result<(Vec<u8>, Vec<String>)> {
    let ops: Vec<Op> = serde_json::from_value(ops.clone())
        .map_err(|e| DocError::Invalid(format!("operations are not valid: {e}")))?;
    if ops.is_empty() || ops.len() > 200 {
        return invalid("give between 1 and 200 operations");
    }
    let mut root = load_document(bytes)?;
    let mut summary = Vec::new();
    for (number, op) in ops.into_iter().enumerate() {
        let done = apply(&mut root, op).map_err(|e| match e {
            DocError::Invalid(m) => DocError::Invalid(format!("operation {}: {m}", number + 1)),
            other => other,
        })?;
        summary.push(format!("{}. {done}", number + 1));
    }
    Ok((rebuild(bytes, &root)?, summary))
}

fn paragraph_ref(refs: &[ParaRef], n: usize) -> Result<&ParaRef> {
    n.checked_sub(1)
        .and_then(|i| refs.get(i))
        .ok_or_else(|| DocError::Invalid(format!("there is no paragraph {n}")))
}

fn apply(root: &mut Element, op: Op) -> Result<String> {
    match op {
        Op::Replace {
            find,
            replace,
            all,
            match_case,
            paragraph,
        } => {
            if find.is_empty() {
                return invalid("find must not be empty");
            }
            let refs = paragraphs(root);
            let targets: Vec<ParaRef> = match paragraph {
                Some(n) => vec![paragraph_ref(&refs, n)?.clone()],
                None => refs,
            };
            let mut total = 0;
            for r in targets {
                let p = at_mut(root, &r.path);
                total +=
                    replace_in_paragraph(p, &find, &replace, match_case, !all && total > 0, all);
                if !all && total > 0 {
                    break;
                }
            }
            if total == 0 {
                return invalid(format!("\"{find}\" was not found"));
            }
            Ok(format!("replaced {total} occurrence(s)"))
        }
        Op::Delete { paragraph } => {
            let refs = paragraphs(root);
            let r = paragraph_ref(&refs, paragraph)?.clone();
            if r.table.is_some() {
                let p = at_mut(root, &r.path);
                p.children
                    .retain(|n| !matches!(n, Node::El(e) if e.name == "w:r"));
                return Ok(format!("cleared table paragraph {paragraph}"));
            }
            let (last, parent) = r.path.split_last().expect("paths are not empty");
            at_mut(root, parent).children.remove(*last);
            Ok(format!("deleted paragraph {paragraph}"))
        }
        Op::Insert { after, blocks } => {
            let blocks = Block::parse_all(&blocks)?;
            if blocks.is_empty() {
                return invalid("insert needs at least one block");
            }
            let mut ctx = Ctx {
                text_width: section_text_width(root),
                ordered_lists: None,
                direct_headings: true,
            };
            let fragment = format!(
                "<w:body xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\">{}</w:body>",
                render_blocks(&blocks, &mut ctx)
            );
            let new_nodes = xml::parse(fragment.as_bytes())?.children;
            let count = new_nodes.len();
            match after {
                Some(n) => {
                    let refs = paragraphs(root);
                    let r = paragraph_ref(&refs, n)?;
                    if r.table.is_some() {
                        return invalid(
                            "cannot insert inside a table; pick a paragraph outside it",
                        );
                    }
                    let (last, parent) = r.path.split_last().expect("paths are not empty");
                    let parent = at_mut(root, parent);
                    for (offset, node) in new_nodes.into_iter().enumerate() {
                        parent.children.insert(last + 1 + offset, node);
                    }
                    Ok(format!("inserted {count} element(s) after paragraph {n}"))
                }
                None => {
                    let body = root.child_mut("w:body").expect("checked on load");
                    let at = body
                        .children
                        .iter()
                        .rposition(|n| matches!(n, Node::El(e) if e.name == "w:sectPr"))
                        .unwrap_or(body.children.len());
                    for (offset, node) in new_nodes.into_iter().enumerate() {
                        body.children.insert(at + offset, node);
                    }
                    Ok(format!("appended {count} element(s) at the end"))
                }
            }
        }
        Op::Style {
            paragraph,
            find,
            all,
            mut style,
        } => {
            style.validate()?;
            if paragraph.is_none() && find.is_none() && !all {
                return invalid("style needs paragraph, find, or all: true");
            }
            let refs = paragraphs(root);
            let targets: Vec<ParaRef> = match (paragraph, &find) {
                (Some(n), _) => vec![paragraph_ref(&refs, n)?.clone()],
                (None, Some(needle)) => refs
                    .iter()
                    .filter(|r| paragraph_text(at(root, &r.path)).contains(needle.as_str()))
                    .cloned()
                    .collect(),
                (None, None) => refs,
            };
            if targets.is_empty() {
                return invalid("no paragraph matched");
            }
            for r in &targets {
                style_paragraph(at_mut(root, &r.path), &style);
            }
            Ok(format!("styled {} paragraph(s)", targets.len()))
        }
    }
}

fn section_text_width(root: &Element) -> u32 {
    let sect = root.child("w:body").and_then(|b| {
        b.children.iter().rev().find_map(|n| match n {
            Node::El(e) if e.name == "w:sectPr" => Some(e),
            _ => None,
        })
    });
    let num = |e: Option<&Element>, name: &str| e.and_then(|e| e.attr(name)?.parse::<u32>().ok());
    let size = sect.and_then(|s| s.child("w:pgSz"));
    let mar = sect.and_then(|s| s.child("w:pgMar"));
    match (num(size, "w:w"), num(mar, "w:left"), num(mar, "w:right")) {
        (Some(w), Some(l), Some(r)) if w > l + r + 1440 => w - l - r,
        _ => 9026,
    }
}

fn replace_in_paragraph(
    p: &mut Element,
    find: &str,
    replacement: &str,
    match_case: bool,
    skip: bool,
    all: bool,
) -> usize {
    if skip {
        return 0;
    }
    let paths: Vec<Vec<usize>> = text_paths(p)
        .into_iter()
        .filter(|(_, kind)| *kind == "t")
        .map(|(path, _)| path)
        .collect();
    let mut texts: Vec<String> = paths.iter().map(|path| at(p, path).own_text()).collect();
    let joined: String = texts.concat();
    let (hay, needle) = if match_case {
        (joined.clone(), find.to_owned())
    } else {
        (joined.to_ascii_lowercase(), find.to_ascii_lowercase())
    };
    let mut matches: Vec<usize> = hay.match_indices(&needle).map(|(i, _)| i).collect();
    if !all {
        matches.truncate(1);
    }
    if matches.is_empty() {
        return 0;
    }
    let mut starts = Vec::with_capacity(texts.len());
    let mut offset = 0;
    for t in &texts {
        starts.push(offset);
        offset += t.len();
    }
    let end_of = |i: usize| starts[i] + texts_len(&starts, offset, i);
    for &s in matches.iter().rev() {
        let e = s + find.len();
        let first = (0..texts.len())
            .find(|&i| s >= starts[i] && s < end_of(i))
            .unwrap_or(0);
        let last = (0..texts.len())
            .rfind(|&i| e > starts[i] && e <= end_of(i))
            .unwrap_or(first);
        if first == last {
            texts[first].replace_range(s - starts[first]..e - starts[first], replacement);
        } else {
            let head = texts[first][..s - starts[first]].to_owned();
            texts[first] = format!("{head}{replacement}");
            for text in &mut texts[first + 1..last] {
                text.clear();
            }
            texts[last] = texts[last][e - starts[last]..].to_owned();
        }
    }
    for (path, text) in paths.iter().zip(&texts) {
        at_mut(p, path).set_text(text);
    }
    matches.len()
}

/// Length of text `i`, from the start offsets of all texts and the total.
fn texts_len(starts: &[usize], total: usize, i: usize) -> usize {
    starts.get(i + 1).copied().unwrap_or(total) - starts[i]
}

const RPR_ORDER: &[&str] = &[
    "w:rStyle",
    "w:rFonts",
    "w:b",
    "w:bCs",
    "w:i",
    "w:iCs",
    "w:caps",
    "w:smallCaps",
    "w:strike",
    "w:dstrike",
    "w:outline",
    "w:shadow",
    "w:emboss",
    "w:imprint",
    "w:noProof",
    "w:snapToGrid",
    "w:vanish",
    "w:webHidden",
    "w:color",
    "w:spacing",
    "w:w",
    "w:kern",
    "w:position",
    "w:sz",
    "w:szCs",
    "w:highlight",
    "w:u",
    "w:effect",
    "w:bdr",
    "w:shd",
    "w:fitText",
    "w:vertAlign",
    "w:rtl",
    "w:cs",
    "w:em",
    "w:lang",
    "w:eastAsianLayout",
    "w:specVanish",
    "w:oMath",
];

const PPR_ORDER: &[&str] = &[
    "w:pStyle",
    "w:keepNext",
    "w:keepLines",
    "w:pageBreakBefore",
    "w:framePr",
    "w:widowControl",
    "w:numPr",
    "w:suppressLineNumbers",
    "w:pBdr",
    "w:shd",
    "w:tabs",
    "w:suppressAutoHyphens",
    "w:kinsoku",
    "w:wordWrap",
    "w:overflowPunct",
    "w:topLinePunct",
    "w:autoSpaceDE",
    "w:autoSpaceDN",
    "w:bidi",
    "w:adjustRightInd",
    "w:snapToGrid",
    "w:spacing",
    "w:ind",
    "w:contextualSpacing",
    "w:mirrorIndents",
    "w:suppressOverlap",
    "w:jc",
    "w:textDirection",
    "w:textAlignment",
    "w:textboxTightWrap",
    "w:outlineLvl",
    "w:divId",
    "w:cnfStyle",
    "w:rPr",
    "w:sectPr",
    "w:pPrChange",
];

/// Set `child` in `parent`, replacing a same-named element and keeping the
/// schema's child order so Word accepts the file.
fn set_ordered(parent: &mut Element, child: Element, order: &[&str]) {
    parent
        .children
        .retain(|n| !matches!(n, Node::El(e) if e.name == child.name));
    let rank = |name: &str| order.iter().position(|o| *o == name).unwrap_or(order.len());
    let mine = rank(&child.name);
    let at = parent
        .children
        .iter()
        .position(|n| matches!(n, Node::El(e) if rank(&e.name) > mine))
        .unwrap_or(parent.children.len());
    parent.children.insert(at, Node::El(child));
}

fn el(name: &str, attrs: &[(&str, &str)]) -> Element {
    Element {
        name: name.to_owned(),
        attrs: attrs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        children: Vec::new(),
    }
}

fn style_paragraph(p: &mut Element, style: &Style) {
    if let Some(align) = style.align {
        if p.child("w:pPr").is_none() {
            p.children.insert(0, Node::El(Element::new("w:pPr")));
        }
        let ppr = p.child_mut("w:pPr").expect("just ensured");
        set_ordered(ppr, el("w:jc", &[("w:val", jc(align))]), PPR_ORDER);
    }
    let touches_runs = style.font.is_some()
        || style.size.is_some()
        || style.bold.is_some()
        || style.italic.is_some()
        || style.underline.is_some()
        || style.color.is_some();
    if !touches_runs {
        return;
    }
    for node in &mut p.children {
        let Node::El(run) = node else { continue };
        if run.name != "w:r" {
            continue;
        }
        if run.child("w:rPr").is_none() {
            run.children.insert(0, Node::El(Element::new("w:rPr")));
        }
        let rpr = run.child_mut("w:rPr").expect("just ensured");
        if let Some(font) = &style.font {
            let mut f = el("w:rFonts", &[]);
            for key in ["w:ascii", "w:hAnsi", "w:cs", "w:eastAsia"] {
                f.set_attr(key, font);
            }
            set_ordered(rpr, f, RPR_ORDER);
        }
        for (flag, tag, tag_cs) in [
            (style.bold, "w:b", Some("w:bCs")),
            (style.italic, "w:i", Some("w:iCs")),
        ] {
            if let Some(on) = flag {
                let val = if on { "1" } else { "0" };
                set_ordered(rpr, el(tag, &[("w:val", val)]), RPR_ORDER);
                if let Some(cs) = tag_cs {
                    set_ordered(rpr, el(cs, &[("w:val", val)]), RPR_ORDER);
                }
            }
        }
        if let Some(color) = &style.color {
            set_ordered(rpr, el("w:color", &[("w:val", color)]), RPR_ORDER);
        }
        if let Some(size) = style.size {
            let half = ((size * 2.0).round() as u32).to_string();
            set_ordered(rpr, el("w:sz", &[("w:val", &half)]), RPR_ORDER);
            set_ordered(rpr, el("w:szCs", &[("w:val", &half)]), RPR_ORDER);
        }
        if let Some(underline) = style.underline {
            let val = if underline { "single" } else { "none" };
            set_ordered(rpr, el("w:u", &[("w:val", val)]), RPR_ORDER);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample() -> Vec<u8> {
        let spec = Spec::parse(&json!({
            "title": "Quarterly report",
            "author": "Lia",
            "page_numbers": true,
            "style": {"font": "Arial", "size": 11},
            "blocks": [
                {"type": "heading", "level": 1, "text": "Summary"},
                {"type": "paragraph", "runs": [
                    {"text": "Revenue grew "}, {"text": "12%", "bold": true}, {"text": " this quarter."}
                ]},
                {"type": "list", "items": ["one", "two"]},
                {"type": "list", "ordered": true, "items": ["first", "second"]},
                {"type": "table", "rows": [["Region", "Sales"], ["North", "10"]]},
                {"type": "page_break"},
                {"type": "paragraph", "text": "R&D <notes> \"quoted\"", "align": "center"}
            ]
        }))
        .unwrap();
        create(&spec).unwrap()
    }

    #[test]
    fn a_created_document_reads_back_with_structure() {
        let text = read(&sample(), 100_000, false).unwrap();
        assert!(text.contains("Quarterly report"));
        assert!(text.contains("# Summary"));
        assert!(text.contains("Revenue grew 12% this quarter."));
        assert!(text.contains("- one"));
        assert!(text.contains("(table 1, row 2, cell 1) North"));
        assert!(text.contains("R&D <notes> \"quoted\""));
    }

    #[test]
    fn replace_spans_runs_and_keeps_formatting_of_the_rest() {
        let (bytes, summary) = edit(
            &sample(),
            &json!([{"op": "replace", "find": "grew 12% this", "replace": "fell 3% last"}]),
        )
        .unwrap();
        assert!(summary[0].contains("replaced 1"));
        let text = read(&bytes, 100_000, true).unwrap();
        assert!(text.contains("Revenue fell 3% last quarter."));
        assert!(text.contains("# Summary"));
        assert!(text.contains("North"));
    }

    #[test]
    fn replace_reports_a_missing_string_and_honours_case() {
        let bad = edit(
            &sample(),
            &json!([{"op": "replace", "find": "nothing here", "replace": "x"}]),
        );
        assert!(bad.is_err());
        let sensitive = edit(
            &sample(),
            &json!([{"op": "replace", "find": "SUMMARY", "replace": "x"}]),
        );
        assert!(sensitive.is_err());
        let loose = edit(
            &sample(),
            &json!([{"op": "replace", "find": "SUMMARY", "replace": "Overview", "match_case": false}]),
        )
        .unwrap();
        assert!(
            read(&loose.0, 100_000, false)
                .unwrap()
                .contains("# Overview")
        );
    }

    #[test]
    fn insert_delete_and_style_change_only_their_targets() {
        let (bytes, _) = edit(
            &sample(),
            &json!([
                {"op": "insert", "after": 2, "blocks": [
                    {"type": "heading", "level": 2, "text": "Added"},
                    {"type": "list", "ordered": true, "items": ["x", "y"]}
                ]},
                {"op": "style", "find": "Revenue", "style": {"italic": true, "color": "#ff0000", "size": 14, "align": "right"}},
                {"op": "delete", "paragraph": 1}
            ]),
        )
        .unwrap();
        let text = read(&bytes, 100_000, true).unwrap();
        assert!(!text.contains("Quarterly report"));
        assert!(text.contains("## Added"));
        assert!(text.contains("1.\tx"));
        let line = text.lines().find(|l| l.contains("Revenue")).unwrap();
        assert!(
            line.contains("italic") && line.contains("color=FF0000") && line.contains("size=14")
        );
        assert!(line.contains("align=right"));
    }

    #[test]
    fn an_edited_package_is_still_a_complete_valid_zip() {
        let (bytes, _) = edit(
            &sample(),
            &json!([{"op": "insert", "blocks": [{"type": "paragraph", "text": "end"}]}]),
        )
        .unwrap();
        let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
        for name in [
            "[Content_Types].xml",
            "word/styles.xml",
            "word/numbering.xml",
            "word/footer1.xml",
        ] {
            assert!(zip.by_name(name).is_ok(), "{name} was lost");
        }
    }

    #[test]
    fn bad_operations_and_files_are_errors() {
        assert!(edit(&sample(), &json!([])).is_err());
        assert!(edit(&sample(), &json!([{"op": "delete", "paragraph": 999}])).is_err());
        assert!(
            edit(
                &sample(),
                &json!([{"op": "style", "style": {"bold": true}}])
            )
            .is_err()
        );
        assert!(read(b"not a zip", 1000, false).is_err());
    }
}
