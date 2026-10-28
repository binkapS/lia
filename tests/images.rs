//! Tests of the pictures the assistant is sent.
//!
//! A picture has to reach the model as an image, and a model that cannot see
//! must not stop the conversation: the picture is described instead, and the
//! turn carries on.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lia::agent::{Agent, AgentConfig, AgentEvent};
use lia::image::Image;
use lia::provider::types::{
    CompletionRequest, CompletionResponse, ContentBlock, Message, ModelInfo, StopReason,
    StreamEvent,
};
use lia::provider::{EventStream, Provider, ProviderError, Result};
use lia::tool::ToolRegistry;
use tokio::sync::mpsc;

/// A provider that records what it was asked, and can be told to refuse
/// pictures the way a model without sight would.
struct SeeingProvider {
    /// The requests the provider received.
    seen: Mutex<Vec<CompletionRequest>>,
    /// Whether the provider refuses pictures.
    blind: bool,
}

impl SeeingProvider {
    fn new(blind: bool) -> Self {
        SeeingProvider {
            seen: Mutex::new(Vec::new()),
            blind,
        }
    }

    fn requests(&self) -> Vec<CompletionRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for SeeingProvider {
    fn name(&self) -> &str {
        "seeing"
    }

    fn model(&self) -> &str {
        "seeing-model"
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.seen.lock().unwrap().push(request.clone());

        // A model that cannot see refuses the picture, the way a provider
        // refuses a request it does not understand.
        if self.blind
            && request.messages.iter().any(|message| {
                message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Image { .. }))
            })
        {
            return Err(ProviderError::InvalidRequest(
                "this model does not support image input".into(),
            ));
        }

        Ok(CompletionResponse {
            content: vec![ContentBlock::text("I can see it.")],
            model: "seeing-model".into(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        let response = self.complete(request).await?;
        let stream = async_stream::stream! {
            for block in response.content {
                if let ContentBlock::Text { text } = block {
                    yield Ok(StreamEvent::text(text));
                }
            }
            yield Ok(StreamEvent::Done {
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            });
        };
        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo::new("seeing-model", 100_000)])
    }
}

/// A picture of a few bytes.
fn picture() -> Image {
    Image::from_bytes(b"\x89PNG\r\n\x1a\n a picture", "image/png").expect("the picture is made")
}

/// An image block, as the channels build it.
fn block(image: &Image) -> ContentBlock {
    ContentBlock::Image {
        media_type: image.media_type.clone(),
        data: image.data.clone(),
    }
}

/// Run a turn and collect the events it produced.
async fn run_turn(
    provider: Arc<SeeingProvider>,
    message: Message,
) -> (Vec<AgentEvent>, Vec<CompletionRequest>) {
    let agent = Agent::new(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
    );
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(32);

    let handle = tokio::spawn(async move { agent.run_turn(vec![message], "test", None, tx).await });

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let _ = handle.await;

    (events, provider.requests())
}

#[tokio::test]
async fn a_picture_reaches_the_model() {
    let image = picture();
    let provider = Arc::new(SeeingProvider::new(false));
    let message = Message::user_with_images("what is this?", vec![block(&image)]);

    let (events, requests) = run_turn(provider, message).await;

    assert_eq!(requests.len(), 1, "the model was not asked");
    let sent = requests[0].messages.last().expect("the message is there");
    let pictures: Vec<&ContentBlock> = sent
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .collect();
    assert_eq!(pictures.len(), 1, "the picture did not reach the model");
    assert!(
        sent.text().contains("what is this?"),
        "the words were lost: {}",
        sent.text()
    );
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "the turn failed: {events:?}"
    );
}

#[tokio::test]
async fn a_model_that_cannot_see_is_asked_again_without_the_picture() {
    let image = picture();
    let provider = Arc::new(SeeingProvider::new(true));
    let message = Message::user_with_images("what is this?", vec![block(&image)]);

    let (events, requests) = run_turn(provider, message).await;

    assert_eq!(requests.len(), 2, "the model was not asked again");
    let second = requests[1].messages.last().expect("the message is there");
    assert!(
        !second
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. })),
        "the picture was sent again"
    );
    assert!(
        second.text().contains("a picture was attached"),
        "the picture was not described: {}",
        second.text()
    );
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Error(_))),
        "the turn failed: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Notice(_))),
        "nothing was said about the picture being dropped"
    );
}

#[tokio::test]
async fn a_model_that_cannot_see_still_answers() {
    let image = picture();
    let provider = Arc::new(SeeingProvider::new(true));
    let message = Message::user_with_images("what is this?", vec![block(&image)]);

    let (events, _requests) = run_turn(provider, message).await;

    let answered = events.iter().any(
        |event| matches!(event, AgentEvent::TextDelta(text) if text.contains("I can see it.")),
    );
    assert!(answered, "the model did not answer: {events:?}");
}

#[tokio::test]
async fn a_file_that_is_not_a_picture_is_refused() {
    let dir = std::env::temp_dir().join(format!("lia-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("the directory is made");
    let path = dir.join("notes.txt");
    std::fs::write(&path, "just words").expect("the file is written");

    let error = Image::from_file(&path).expect_err("a text file is not a picture");
    assert!(error.contains("not a picture"), "{error}");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_picture_is_read_from_a_file() {
    let dir = std::env::temp_dir().join(format!("lia-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("the directory is made");
    let path = dir.join("shot.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n a picture").expect("the file is written");

    let image = Image::from_file(&path).expect("the picture is read");
    assert_eq!(image.media_type, "image/png");
    assert!(!image.data.is_empty());

    let _ = std::fs::remove_dir_all(dir);
}
