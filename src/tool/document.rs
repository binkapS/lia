//! Tools that read, create and edit PDF and Word (.docx) documents.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::filesystem::Workspace;
use super::{Result, Tool, ToolError, ToolOutput};
use crate::document::{self, DocError, Format, Spec};

const MAX_INPUT_BYTES: u64 = 50 * 1024 * 1024;
const DEFAULT_MAX_CHARS: usize = 20_000;
const HARD_MAX_CHARS: usize = 200_000;

fn failed(tool: &str, error: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(tool.into(), error.to_string())
}

fn invalid(tool: &str, reason: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs {
        tool: tool.into(),
        reason: reason.into(),
    }
}

fn convert(tool: &str, error: DocError) -> ToolError {
    match error {
        DocError::Invalid(reason) => invalid(tool, reason),
        other => failed(tool, other),
    }
}

fn required<'a>(tool: &str, args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(tool, format!("missing required parameter '{key}'")))
}

async fn load(tool: &str, path: &Path) -> Result<Vec<u8>> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| failed(tool, format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(failed(tool, format!("{} is not a file", path.display())));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(failed(
            tool,
            format!(
                "the file is larger than {} MB",
                MAX_INPUT_BYTES / 1024 / 1024
            ),
        ));
    }
    tokio::fs::read(path)
        .await
        .map_err(|e| failed(tool, e.to_string()))
}

/// Write through a temporary file so a failure never leaves a torn document.
async fn store(tool: &str, path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| failed(tool, e.to_string()))?;
    }
    if !overwrite && path.exists() {
        return Err(failed(
            tool,
            format!(
                "{} already exists; set overwrite to replace it",
                path.display()
            ),
        ));
    }
    let mut temp: PathBuf = path.to_path_buf();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    temp.set_file_name(format!(".{name}.lia-tmp"));
    tokio::fs::write(&temp, bytes)
        .await
        .map_err(|e| failed(tool, e.to_string()))?;
    if let Err(e) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(failed(tool, e.to_string()));
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(
    tool: &str,
    work: impl FnOnce() -> document::Result<T> + Send + 'static,
) -> Result<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(|e| convert(tool, e)),
        Err(e) => Err(failed(tool, format!("document processing crashed: {e}"))),
    }
}

fn format_of(tool: &str, path: &Path) -> Result<Format> {
    Format::of(path).map_err(|e| convert(tool, e))
}

/// Read the text of a PDF or .docx file.
pub struct DocumentReadTool {
    workspace: Workspace,
}

impl DocumentReadTool {
    pub fn new(workspace: Workspace) -> Self {
        DocumentReadTool { workspace }
    }
}

#[async_trait]
impl Tool for DocumentReadTool {
    fn name(&self) -> &str {
        "document_read"
    }

    fn description(&self) -> &str {
        "Read a .pdf or .docx file as text. DOCX output numbers every paragraph as [n] (the numbers document_edit uses), marks headings with #, list items with -, and table cells with their position; set formatting to also see fonts, sizes, bold/italic and alignment. PDFs are returned page by page."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to a .pdf or .docx file." },
                "max_chars": { "type": "integer", "description": "Maximum characters to return (default 20000)." },
                "pages": { "type": "array", "items": { "type": "integer" }, "description": "PDF only: 1-based page numbers to read. Default is every page." },
                "formatting": { "type": "boolean", "description": "DOCX only: include style details. Default false." }
            },
            "required": ["path"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let name = self.name();
        let path = self.workspace.resolve(required(name, &args, "path")?)?;
        let format = format_of(name, &path)?;
        let max_chars = args
            .get("max_chars")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MAX_CHARS, |n| {
                (n as usize).clamp(100, HARD_MAX_CHARS)
            });
        let pages: Option<Vec<u32>> = match args.get("pages") {
            None | Some(Value::Null) => None,
            Some(Value::Array(list)) => Some(
                list.iter()
                    .map(|v| {
                        v.as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .filter(|n| *n >= 1)
                            .ok_or_else(|| invalid(name, "pages are 1-based integers"))
                    })
                    .collect::<Result<_>>()?,
            ),
            Some(_) => return Err(invalid(name, "pages must be an array of integers")),
        };
        let formatting = args
            .get("formatting")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let bytes = load(name, &path).await?;
        let text = blocking(name, move || match format {
            Format::Pdf => document::pdf::read(&bytes, pages.as_deref(), max_chars),
            Format::Docx => document::docx::read(&bytes, max_chars, formatting),
        })
        .await?;
        Ok(ToolOutput::ok(text).with_metadata(json!({
            "path": self.workspace.display(&path),
            "format": if format == Format::Pdf { "pdf" } else { "docx" },
        })))
    }
}

const SPEC_HELP: &str = "Document spec: {title?, author?, style?: {font, size, bold, italic, underline, color (hex), align}, page_size?: \"a4\"|\"letter\", margin?: points, page_numbers?: bool, blocks: [...]}. Block types: {type:\"heading\", level:1-3, text|runs}, {type:\"paragraph\", text|runs, ...style}, {type:\"list\", ordered?, items:[text]}, {type:\"table\", rows:[[text]], header?: true}, {type:\"page_break\"}. runs is [{text, bold?, italic?, underline?, color?, size?, font?}] for mixed styling inside one paragraph.";

/// Create a new PDF or .docx file from a structured description.
pub struct DocumentCreateTool {
    workspace: Workspace,
}

impl DocumentCreateTool {
    pub fn new(workspace: Workspace) -> Self {
        DocumentCreateTool { workspace }
    }
}

#[async_trait]
impl Tool for DocumentCreateTool {
    fn name(&self) -> &str {
        "document_create"
    }

    fn description(&self) -> &str {
        "Create a styled .pdf or .docx document (the extension picks the format) from headings, paragraphs with styled runs, lists, tables and page breaks. PDFs use the standard fonts (Helvetica, Times, Courier); characters outside Western European text are replaced with '?'. Prefer .docx for documents that will be edited later. For richly designed PDFs (CSS layout, colors, typography, full Unicode), pass a styled 'html' string instead of 'spec'; it is rendered with headless Chrome/Chromium, wkhtmltopdf or WeasyPrint."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Where to write, ending in .pdf or .docx." },
                "spec": { "type": "object", "description": SPEC_HELP },
                "html": { "type": "string", "description": "PDF only, instead of spec: a complete styled HTML document with inline <style> CSS (full CSS layout, colors, fonts, tables, images as data: URIs). Scripts and network/file access are blocked. Use page_size and margin to control the page." },
                "page_size": { "type": "string", "enum": ["a4", "letter"], "description": "With html: page size. Default a4." },
                "margin": { "type": "number", "description": "With html: page margin in points. Default 72." },
                "overwrite": { "type": "boolean", "description": "Replace an existing file. Default false." }
            },
            "required": ["path"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let name = self.name();
        let path = self.workspace.resolve(required(name, &args, "path")?)?;
        let format = format_of(name, &path)?;
        let html = args.get("html").and_then(Value::as_str).map(str::to_owned);
        if html.is_some() && format != Format::Pdf {
            return Err(invalid(name, "html can only be used with a .pdf path"));
        }
        let spec_value = match (&html, args.get("spec")) {
            (Some(_), Some(_)) => return Err(invalid(name, "give either spec or html, not both")),
            (Some(_), None) => Value::Null,
            (None, Some(spec)) => spec.clone(),
            (None, None) => {
                return Err(invalid(
                    name,
                    "missing required parameter 'spec' (or 'html')",
                ));
            }
        };
        let overwrite = args
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !overwrite && path.exists() {
            return Err(failed(
                name,
                format!(
                    "{} already exists; set overwrite to replace it",
                    self.workspace.display(&path)
                ),
            ));
        }
        let bytes = if let Some(html) = html {
            let page: document::PageSize = match args.get("page_size") {
                None | Some(Value::Null) => Default::default(),
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|_| invalid(name, "page_size must be \"a4\" or \"letter\""))?,
            };
            let margin = args.get("margin").and_then(Value::as_f64).unwrap_or(72.0) as f32;
            if !(18.0..=200.0).contains(&margin) {
                return Err(invalid(name, "margin must be between 18 and 200 points"));
            }
            document::html::render(&html, page, margin)
                .await
                .map_err(|e| convert(name, e))?
        } else {
            blocking(name, move || {
                let spec = Spec::parse(&spec_value)?;
                match format {
                    Format::Pdf => document::pdf::create(&spec),
                    Format::Docx => document::docx::create(&spec),
                }
            })
            .await?
        };
        store(name, &path, &bytes, overwrite).await?;
        Ok(ToolOutput::ok(format!(
            "Created {} ({} bytes)",
            self.workspace.display(&path),
            bytes.len()
        ))
        .with_metadata(json!({
            "path": self.workspace.display(&path),
            "bytes": bytes.len(),
        })))
    }
}

/// Edit an existing PDF or .docx file.
pub struct DocumentEditTool {
    workspace: Workspace,
}

impl DocumentEditTool {
    pub fn new(workspace: Workspace) -> Self {
        DocumentEditTool { workspace }
    }
}

#[async_trait]
impl Tool for DocumentEditTool {
    fn name(&self) -> &str {
        "document_edit"
    }

    fn description(&self) -> &str {
        "Edit a .pdf or .docx file with a list of operations, applied in order and saved only if all succeed. DOCX ops: {op:\"replace\", find, replace, all?, match_case?, paragraph?}, {op:\"delete\", paragraph}, {op:\"insert\", after?: paragraph, blocks:[...]}, {op:\"style\", paragraph|find|all, style:{font,size,bold,italic,underline,color,align}}. Paragraph numbers come from document_read. PDF ops: {op:\"delete_pages\", pages:[...]}, {op:\"replace_text\", find, replace, page?} (best effort), {op:\"set_metadata\", title?, author?}, {op:\"append\", blocks:[...], style?}. Existing PDFs cannot be restyled; create a .docx or a new PDF instead. Blocks use the same format as document_create."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The .pdf or .docx file to edit." },
                "ops": { "type": "array", "items": { "type": "object" }, "description": "Operations to apply in order." },
                "output": { "type": "string", "description": "Write the result here instead of changing the original." }
            },
            "required": ["path", "ops"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let name = self.name();
        let path = self.workspace.resolve(required(name, &args, "path")?)?;
        let format = format_of(name, &path)?;
        let target = match args.get("output").and_then(Value::as_str) {
            Some(out) => {
                let target = self.workspace.resolve(out)?;
                if format_of(name, &target)? != format {
                    return Err(invalid(name, "output must have the same extension as path"));
                }
                target
            }
            None => path.clone(),
        };
        let ops = args
            .get("ops")
            .ok_or_else(|| invalid(name, "missing required parameter 'ops'"))?
            .clone();
        let bytes = load(name, &path).await?;
        let (edited, notes) = blocking(name, move || match format {
            Format::Pdf => document::pdf::edit(&bytes, &ops),
            Format::Docx => document::docx::edit(&bytes, &ops),
        })
        .await?;
        store(name, &target, &edited, true).await?;
        let shown = self.workspace.display(&target);
        Ok(ToolOutput::ok(format!(
            "Edited {shown} ({} bytes)\n{}",
            edited.len(),
            notes.join("\n")
        ))
        .with_metadata(json!({ "path": shown, "bytes": edited.len(), "notes": notes })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        (dir, ws)
    }

    #[tokio::test]
    async fn create_read_and_edit_a_docx() {
        let (_dir, ws) = workspace();
        let create = DocumentCreateTool::new(ws.clone());
        let out = create
            .run(json!({"path": "notes/plan.docx", "spec": {
                "title": "Plan",
                "blocks": [{"type": "paragraph", "text": "Ship on Friday"}]
            }}))
            .await
            .unwrap();
        assert!(out.content.contains("Created"), "{}", out.content);
        let again = create
            .run(json!({"path": "notes/plan.docx", "spec": {"title": "x"}}))
            .await;
        assert!(again.is_err(), "must not overwrite silently");

        let edit = DocumentEditTool::new(ws.clone());
        edit.run(json!({"path": "notes/plan.docx", "ops": [
            {"op": "replace", "find": "Friday", "replace": "Monday"}
        ]}))
        .await
        .unwrap();
        let read = DocumentReadTool::new(ws)
            .run(json!({"path": "notes/plan.docx"}))
            .await
            .unwrap();
        assert!(read.content.contains("Ship on Monday"), "{}", read.content);
    }

    #[tokio::test]
    async fn create_read_and_append_to_a_pdf() {
        let (_dir, ws) = workspace();
        DocumentCreateTool::new(ws.clone())
            .run(json!({"path": "a.pdf", "spec": {
                "title": "Report", "blocks": [{"type": "paragraph", "text": "First page"}]
            }}))
            .await
            .unwrap();
        DocumentEditTool::new(ws.clone())
            .run(json!({"path": "a.pdf", "output": "b.pdf", "ops": [
                {"op": "append", "blocks": [{"type": "paragraph", "text": "Second"}]}
            ]}))
            .await
            .unwrap();
        let read = DocumentReadTool::new(ws)
            .run(json!({"path": "b.pdf", "pages": [2]}))
            .await
            .unwrap();
        assert!(read.content.contains("Second"), "{}", read.content);
    }

    #[tokio::test]
    async fn bad_input_is_rejected_cleanly() {
        let (dir, ws) = workspace();
        let read = DocumentReadTool::new(ws.clone());
        assert!(read.run(json!({"path": "missing.pdf"})).await.is_err());
        assert!(read.run(json!({"path": "notes.txt"})).await.is_err());
        assert!(
            read.run(json!({"path": "../../etc/passwd.pdf"}))
                .await
                .is_err()
        );
        std::fs::write(dir.path().join("broken.docx"), b"not a zip").unwrap();
        assert!(read.run(json!({"path": "broken.docx"})).await.is_err());
        let create = DocumentCreateTool::new(ws);
        assert!(
            create
                .run(json!({"path": "x.pdf", "spec": {"blocks": []}}))
                .await
                .is_err()
        );
        assert!(!dir.path().join("x.pdf").exists());
    }
}
