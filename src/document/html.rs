//! Render styled, self-contained HTML to PDF with an installed engine
//! (headless Chrome/Chromium, wkhtmltopdf or WeasyPrint).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;

use super::{DocError, PageSize, Result, invalid};

pub const MAX_HTML_BYTES: usize = 5 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

/// Blocks network, file and script access: the page must be self-contained
/// (inline CSS, images as data: URIs).
const CSP: &str = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:\">";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Engine {
    Chrome,
    Wkhtmltopdf,
    Weasyprint,
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

fn find_engine() -> Option<(Engine, PathBuf)> {
    const CANDIDATES: [(&str, Engine); 7] = [
        ("google-chrome", Engine::Chrome),
        ("google-chrome-stable", Engine::Chrome),
        ("chromium", Engine::Chrome),
        ("chromium-browser", Engine::Chrome),
        ("chrome", Engine::Chrome),
        ("wkhtmltopdf", Engine::Wkhtmltopdf),
        ("weasyprint", Engine::Weasyprint),
    ];
    CANDIDATES
        .iter()
        .find_map(|(name, engine)| find_in_path(name).map(|p| (*engine, p)))
}

/// Add the security policy and default page rules to the HTML.
fn prepare(html: &str, page: PageSize, margin: f32) -> String {
    let (w, h) = page.points();
    let css = format!("<style>@page {{ size: {w}pt {h}pt; margin: {margin}pt; }}</style>");
    let head = format!("<meta charset=\"utf-8\">{CSP}");
    let lower = html.to_ascii_lowercase();
    if let Some(at) = lower.find("<head>").map(|i| i + "<head>".len()) {
        // The page's own <style> comes later and wins over our defaults.
        return format!("{}{head}{css}{}", &html[..at], &html[at..]);
    }
    if let Some(at) = lower
        .find("<html")
        .and_then(|i| lower[i..].find('>').map(|j| i + j + 1))
    {
        return format!("{}<head>{head}{css}</head>{}", &html[..at], &html[at..]);
    }
    format!("<!DOCTYPE html><html><head>{head}{css}</head><body>{html}</body></html>")
}

fn io(e: impl std::fmt::Display) -> DocError {
    DocError::Io(e.to_string())
}

/// Convert HTML to PDF bytes.
pub async fn render(html: &str, page: PageSize, margin: f32) -> Result<Vec<u8>> {
    if html.trim().is_empty() {
        return invalid("html must not be empty");
    }
    if html.len() > MAX_HTML_BYTES {
        return invalid("the html is too large");
    }
    let (engine, program) = find_engine().ok_or_else(|| {
        DocError::Unsupported(
            "no HTML-to-PDF engine found; install Google Chrome/Chromium, wkhtmltopdf or WeasyPrint"
                .into(),
        )
    })?;

    let dir = std::env::temp_dir().join(format!("lia-html-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await.map_err(io)?;
    let result = run(engine, &program, &dir, &prepare(html, page, margin), page).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn run(
    engine: Engine,
    program: &Path,
    dir: &Path,
    html: &str,
    page: PageSize,
) -> Result<Vec<u8>> {
    let input = dir.join("input.html");
    let output = dir.join("output.pdf");
    tokio::fs::write(&input, html).await.map_err(io)?;

    let mut cmd = Command::new(program);
    match engine {
        Engine::Chrome => {
            let profile = dir.join("profile");
            cmd.arg("--headless")
                .arg("--disable-gpu")
                .arg("--no-sandbox")
                .arg("--disable-extensions")
                .arg("--no-pdf-header-footer")
                .arg("--blink-settings=scriptEnabled=false")
                .arg(format!("--user-data-dir={}", profile.display()))
                .arg(format!("--print-to-pdf={}", output.display()))
                .arg(format!("file://{}", input.display()));
        }
        Engine::Wkhtmltopdf => {
            let size = if page == PageSize::A4 { "A4" } else { "Letter" };
            cmd.arg("--disable-javascript")
                .arg("--disable-local-file-access")
                .arg("--quiet")
                .arg("--page-size")
                .arg(size)
                .arg(&input)
                .arg(&output);
        }
        Engine::Weasyprint => {
            cmd.arg(&input).arg(&output);
        }
    }
    cmd.current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| io("the HTML renderer timed out"))?
        .map_err(|e| io(format!("could not start {}: {e}", program.display())))?;

    match tokio::fs::read(&output).await {
        Ok(bytes) if bytes.starts_with(b"%PDF") => Ok(bytes),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let start = stderr.len().saturating_sub(500);
            let tail = stderr.get(start..).unwrap_or("");
            Err(io(format!(
                "the HTML renderer produced no PDF: {}",
                tail.trim()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_injects_policy_in_every_shape() {
        for html in [
            "<p>x</p>",
            "<html><body>x</body></html>",
            "<html><head><title>t</title></head><body>x</body></html>",
            "<HTML><HEAD></HEAD><BODY>x</BODY></HTML>",
        ] {
            let out = prepare(html, PageSize::A4, 72.0);
            assert!(out.contains("Content-Security-Policy"), "{html}");
            assert!(out.contains("@page"), "{html}");
        }
    }

    #[tokio::test]
    async fn empty_html_is_rejected() {
        assert!(render("  ", PageSize::A4, 72.0).await.is_err());
    }
}
