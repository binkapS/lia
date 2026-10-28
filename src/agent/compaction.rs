//! Context compaction.
//!
//! When a conversation approaches the model's context window we cannot just
//! drop it — the model would lose the thread. Instead we summarise the older
//! part of the conversation into a single system-style message and keep the
//! recent turns verbatim.

use crate::agent::AgentConfig;
use crate::provider::Provider;
use crate::provider::types::{CompletionRequest, ContentBlock, Message, Role};

/// Whether the request is close enough to the context limit to compact.
///
/// Returns the estimated token count when compaction is needed.
pub fn should_compact(request: &CompletionRequest, config: &AgentConfig) -> Option<usize> {
    if !config.tools_enabled && request.messages.len() < 20 {
        return None;
    }

    let context_window = if config.context_window == 0 {
        128_000usize
    } else {
        config.context_window
    };
    let estimated = estimate_tokens(request);
    let threshold = (context_window as f64 * config.compaction_threshold) as usize;

    if estimated > threshold {
        Some(estimated)
    } else {
        None
    }
}

/// Rough token count of a request.
fn estimate_tokens(request: &CompletionRequest) -> usize {
    let mut chars = request.system.as_ref().map(|s| s.len()).unwrap_or(0);
    for tool in &request.tools {
        chars += tool.name.len();
        chars += tool.description.len();
        chars += tool.input_schema.to_string().len();
    }
    for message in &request.messages {
        for block in &message.content {
            chars += match block {
                ContentBlock::Text { text } => text.len(),
                ContentBlock::Thinking { text } => text.len(),
                ContentBlock::ToolUse { input, .. } => input.to_string().len(),
                ContentBlock::ToolResult { content, .. } => content.len(),
                ContentBlock::Image { .. } => 8_000,
            };
        }
        chars += 4;
    }
    chars / 4
}

/// Rewrite a conversation so it fits, keeping the most recent turns intact.
///
/// The older turns are replaced with a single message summarising them. If a
/// summary cannot be produced (no provider, or the call failed), the oldest
/// turns are dropped instead, which is lossy but keeps the agent running.
pub async fn compact(
    messages: Vec<Message>,
    _estimated_tokens: usize,
    provider: &dyn Provider,
) -> Vec<Message> {
    // Keep the most recent exchanges verbatim; this is the part the model is
    // actively reasoning about.
    // Keep roughly the most recent half of the turns, but never more than a
    // fixed cap and never none: compaction must always leave at least one
    // recent message beside the summary.
    const KEEP_RECENT: usize = 8;

    if messages.len() <= 1 {
        return messages;
    }

    let keep_recent = (messages.len() / 2).clamp(1, KEEP_RECENT);
    let split_at = messages.len() - keep_recent;
    let (older, recent) = messages.split_at(split_at);

    let transcript = render_transcript(older);
    let summary = summarize(provider, &transcript).await;

    let mut compacted = Vec::new();
    compacted.push(Message {
        role: Role::User,
        content: vec![ContentBlock::text(format!(
            "Summary of the earlier part of this conversation:\n\n{summary}"
        ))],
    });
    compacted.extend(recent.to_vec());
    compacted
}

/// Render messages as a plain-text transcript.
fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        let role = match message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool",
            Role::System => "System",
        };
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => {
                    out.push_str(&format!("{role}: {text}\n\n"));
                }
                ContentBlock::Thinking { text } => {
                    out.push_str(&format!("{role} thought: {text}\n\n"));
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    out.push_str(&format!("{role} called tool {name}: {input}\n\n"));
                }
                ContentBlock::ToolResult { content, .. } => {
                    out.push_str(&format!("Tool result: {content}\n\n"));
                }
                ContentBlock::Image { .. } => {
                    out.push_str(&format!("{role}: [image]\n\n"));
                }
            }
        }
    }
    out
}

async fn choose_compaction_model(provider: &dyn Provider) -> String {
    if let Ok(models) = provider.list_models().await {
        for preferred in ["haiku", "mini", "small", "flash", "lite"] {
            for model in &models {
                let name = model.id.to_lowercase();
                if name.contains(preferred) {
                    return model.id.clone();
                }
            }
        }
        if let Some(model) = models.first() {
            return model.id.clone();
        }
    }
    provider.model().to_string()
}

/// Ask the model to summarise the transcript.
///
/// Falls back to a truncated excerpt when the provider is unavailable.
async fn summarize(provider: &dyn Provider, transcript: &str) -> String {
    // Keep the request small; a huge transcript would not fit anyway.
    let excerpt: String = transcript.chars().take(20_000).collect();
    let truncated = excerpt.chars().count() < transcript.chars().count();

    // Prefer a small/cheap model for summarising when the provider can
    // name one, falling back to the configured model otherwise.
    let model = choose_compaction_model(provider).await;
    let request = CompletionRequest {
        model,
        system: Some(
            "Summarise this conversation. Preserve decisions, file paths, names, \
             open questions, and anything the assistant must remember to continue. \
             Be concise and factual."
                .into(),
        ),
        messages: vec![Message::user(excerpt)],
        tools: Vec::new(),
        max_output_tokens: Some(1024),
        temperature: None,
        extra: None,
    };

    match provider.complete(&request).await {
        Ok(response) if !response.text().trim().is_empty() => {
            let text = response.text().trim().to_string();
            if truncated {
                format!("{text}\n\n(transcript was truncated)")
            } else {
                text
            }
        }
        _ => {
            // Provider unavailable: fall back to the head of the transcript.
            let fallback: String = transcript.chars().take(2000).collect();
            format!("{fallback}\n\n(summary unavailable — earlier turns truncated)")
        }
    }
}
