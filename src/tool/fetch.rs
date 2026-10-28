//! Fetch a URL and return its contents.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Result, Tool, ToolError, ToolOutput};

/// Download a URL and expose it as text.
pub struct FetchTool {
    client: reqwest::Client,
    max_bytes: usize,
    user_agent: String,
}

impl FetchTool {
    pub fn new() -> Self {
        FetchTool {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            max_bytes: 500_000,
            user_agent: "lia/0.1 (+https://github.com/…)".to_string(),
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl Default for FetchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Very small HTML-to-text reduction, so the model gets prose rather than
/// markup. Not a full parser — good enough to make a page readable.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip_depth = 0usize;
    let bytes: Vec<char> = html.chars().collect();
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];
        if c == '<' {
            // Detect script/style bodies to skip entirely.
            let rest: String = bytes[i..].iter().take(8).collect::<String>().to_lowercase();
            if rest.starts_with("<script") || rest.starts_with("<style") {
                skip_depth = 1;
                in_tag = true;
                i += 1;
                continue;
            }
            in_tag = true;
            i += 1;
            continue;
        }
        if c == '>' && in_tag {
            in_tag = false;
            if skip_depth > 0 {
                let rest: String = bytes[i..].iter().take(8).collect::<String>().to_lowercase();
                if rest.starts_with("</script") || rest.starts_with("</style") {
                    skip_depth = 0;
                }
            }
            i += 1;
            // A tag boundary is a word boundary; keep the text readable.
            out.push(' ');
            continue;
        }
        if !in_tag && skip_depth == 0 {
            out.push(c);
        }
        i += 1;
    }

    // Collapse runs of whitespace.
    let mut cleaned = String::with_capacity(out.len());
    let mut last_was_space = false;
    for c in out.chars() {
        let is_space = c.is_whitespace();
        if is_space {
            if !last_was_space {
                cleaned.push(' ');
            }
        } else {
            cleaned.push(c);
        }
        last_was_space = is_space;
    }
    cleaned.trim().to_string()
}

#[async_trait]
impl Tool for FetchTool {
    fn name(&self) -> &str {
        "fetch_url"
    }

    fn description(&self) -> &str {
        "Download a URL and return its contents as text. HTML pages are converted to readable text."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to fetch." }
            },
            "required": ["url"]
        })
    }

    async fn run(&self, args: Value) -> Result<ToolOutput> {
        let url =
            args.get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArgs {
                    tool: self.name().into(),
                    reason: "missing required parameter 'url'".into(),
                })?;

        // Only fetch over http(s); block file:// and other local schemes.
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ToolError::Failed(
                self.name().into(),
                "only http and https URLs are supported".into(),
            ));
        }

        let response = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, &self.user_agent)
            .send()
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if !status.is_success() {
            return Err(ToolError::Failed(
                self.name().into(),
                format!("HTTP {}", status.as_u16()),
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|e| ToolError::Failed(self.name().into(), e.to_string()))?;

        let is_html = content_type.contains("html") || body.trim_start().starts_with('<');
        let text = if is_html {
            strip_html(&body)
        } else {
            body.clone()
        };

        let truncated = text.len() > self.max_bytes;
        let text = if truncated {
            let mut end = self.max_bytes;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}\n\n[truncated]", &text[..end])
        } else {
            text
        };

        Ok(ToolOutput::ok(text).with_metadata(json!({
            "url": url,
            "status": status.as_u16(),
            "content_type": content_type,
            "truncated": truncated,
            // The agent uses this to know that something in this turn came
            // from outside the machine, and cannot be taken at face value.
            "source": "network",
        })))
    }
}

use std::time::Duration;
