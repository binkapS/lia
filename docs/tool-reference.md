# Tool reference

## Filesystem
- `read_file`, `write_file`, `edit_file`, `list_directory`, `search`
- `copy_path`, `move_path`, `patch`, `format_file`
- `grep_codebase`, `find_symbols`

## Documents
- `document_read`: read `.pdf` (per page) or `.docx` (numbered paragraphs, optional formatting).
- `document_create` (approval): build a styled `.pdf`/`.docx` from headings, styled runs, lists, tables, page breaks. For `.pdf`, pass `html` (styled HTML with inline CSS, optional `page_size`/`margin`) instead of `spec` to render via headless Chrome/Chromium, wkhtmltopdf or WeasyPrint (scripts and network/file access blocked; images as `data:` URIs).
- `document_edit` (approval): DOCX replace/delete/insert/style by paragraph number or text; PDF delete pages, best-effort text replace, metadata, append pages. Atomic write; `output` keeps the original.
- Limits: PDFs use the 14 standard fonts (non-Latin text becomes `?`); existing PDFs cannot be restyled; `.doc` is unsupported; DOCX headers/footers and text boxes are not editable.

## Shell
- `run_shell`

## Web
- `fetch_url`, `web_search`, `read_url_chunked`

## Memory
- `memory_search`, `memory_get`, `remember`, `memory_upsert`, `memory_import`, `memory_export`

## Voice
- `listen`, `speak`, `voice_health`

## Tasks
- `task` tooling
