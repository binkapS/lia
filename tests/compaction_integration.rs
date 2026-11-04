use async_trait::async_trait;
use futures_util::stream;
use lia::agent::compaction;
use lia::provider::EventStream;
use lia::provider::types::{CompletionRequest, CompletionResponse, ContentBlock, Message, Role};
use lia::provider::{Provider, ProviderError};

struct FakeProvider;

#[async_trait]
impl Provider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }
    fn model(&self) -> &str {
        "fake-model"
    }
    async fn complete(
        &self,
        _request: &CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(CompletionResponse {
            content: vec![ContentBlock::Text {
                text: "summary".into(),
            }],
            model: "fake".into(),
            stop_reason: None,
            usage: None,
        })
    }
    async fn stream(&self, _request: &CompletionRequest) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(stream::empty()))
    }
}

#[tokio::test]
async fn compaction_smoke() {
    let messages: Vec<Message> = (0..20)
        .map(|i| Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!("msg {i}"),
            }],
        })
        .collect();
    let req = CompletionRequest {
        model: "fake".into(),
        system: None,
        messages: messages.clone(),
        tools: vec![],
        max_output_tokens: None,
        temperature: None,
        extra: None,
    };
    let tokens = compaction::should_compact(&req, &lia::agent::AgentConfig::default());
    // compaction::compact
    let compacted = compaction::compact(messages, tokens.unwrap_or(0), &FakeProvider).await;
    assert!(compacted.len() < 21);
    assert!(!compacted.is_empty());
}
