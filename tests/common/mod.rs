//! Helpers shared by the tests.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lia::provider::types::{
    CompletionRequest, CompletionResponse, ContentBlock, StopReason, StreamEvent,
};
use lia::provider::{EventStream, Provider, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A provider that answers the same way, and remembers what it was asked.
///
/// It can also be told to ask for a tool first, so the approval path can be
/// exercised.
pub struct MockProvider {
    /// The requests the provider received.
    seen: Mutex<Vec<CompletionRequest>>,
    /// The tools to ask for, in order, before answering.
    tools: Mutex<Vec<(String, serde_json::Value)>>,
    /// What the provider says when it is not asking for a tool.
    answer: String,
}

impl MockProvider {
    pub fn new() -> Self {
        MockProvider {
            seen: Mutex::new(Vec::new()),
            tools: Mutex::new(Vec::new()),
            answer: "the answer".to_string(),
        }
    }

    /// A provider that asks for a tool before it answers.
    pub fn with_tool(name: &str, input: serde_json::Value) -> Self {
        Self::with_tools(vec![(name.to_string(), input)])
    }

    /// A provider that asks for a list of tools, one on each turn, before
    /// it answers.
    pub fn with_tools(tools: Vec<(String, serde_json::Value)>) -> Self {
        MockProvider {
            seen: Mutex::new(Vec::new()),
            tools: Mutex::new(tools),
            answer: "the answer".to_string(),
        }
    }

    /// A provider that always says the same thing.
    pub fn answering(text: &str) -> Self {
        MockProvider {
            seen: Mutex::new(Vec::new()),
            tools: Mutex::new(Vec::new()),
            answer: text.to_string(),
        }
    }

    /// How many requests the provider has answered.
    pub fn requests(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// The requests the provider has answered.
    pub fn seen(&self) -> Vec<CompletionRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        MockProvider::new()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.seen.lock().unwrap().push(request.clone());

        // The first requests ask for the tools; the next one answers.
        let mut tools = self.tools.lock().unwrap();
        if let Some((name, input)) = tools.first().cloned() {
            tools.remove(0);
            return Ok(CompletionResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name,
                    input,
                }],
                model: "mock-model".into(),
                stop_reason: Some(StopReason::ToolUse),
                usage: None,
            });
        }

        Ok(CompletionResponse {
            content: vec![ContentBlock::text(self.answer.clone())],
            model: "mock-model".into(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        })
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream> {
        let response = self.complete(request).await?;
        let stream = async_stream::stream! {
            for block in response.content {
                match block {
                    ContentBlock::Text { text } => {
                        yield Ok(StreamEvent::text(text));
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        yield Ok(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
                        yield Ok(StreamEvent::ToolCallDelta {
                            id: id.clone(),
                            arguments: input.to_string(),
                        });
                        yield Ok(StreamEvent::ToolCallEnd { id });
                    }
                    _ => {}
                }
            }
            yield Ok(StreamEvent::Done {
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            });
        };
        Ok(Box::pin(stream))
    }
}

/// A WebSocket client, small enough to live in the test helpers.
///
/// It speaks just enough of RFC 6455 to open a connection to the gateway
/// and trade text frames with it: the handshake, masked frames out, and
/// unfragmented frames in.
pub struct WsClient {
    stream: TcpStream,
    /// Bytes that have been read but not used yet.
    buffer: Vec<u8>,
}

impl WsClient {
    /// Connect to a `ws://host:port/path` address.
    pub async fn connect(url: &str) -> WsClient {
        let rest = url
            .strip_prefix("ws://")
            .expect("the address is not a ws:// one");
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, "/"),
        };

        let mut stream = TcpStream::connect(authority)
            .await
            .expect("could not connect");

        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {authority}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: c2hhbGwtb25seXMxMg==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut client = WsClient {
            stream,
            buffer: Vec::new(),
        };

        // The handshake ends with a blank line.
        while !client.buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut chunk = [0u8; 1024];
            let read = client.stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "the server closed the connection");
            client.buffer.extend_from_slice(&chunk[..read]);
        }

        let head = String::from_utf8_lossy(&client.buffer).to_string();
        assert!(
            head.starts_with("HTTP/1.1 101"),
            "the server refused the upgrade: {head}"
        );

        // Whatever came after the headers belongs to the first frame.
        let end = client
            .buffer
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("no end of headers")
            + 4;
        client.buffer.drain(..end);

        client
    }

    /// Send a text frame.
    pub async fn send_text(&mut self, text: &str) {
        let frame = masked_frame(text.as_bytes(), 0x1);
        self.stream.write_all(&frame).await.unwrap();
    }

    /// Wait for the next text frame.
    pub async fn next_text(&mut self) -> String {
        loop {
            let (opcode, payload) = self.read_frame().await;
            match opcode {
                // A text frame, or the end of one.
                0x1 | 0x0 => {
                    if opcode == 0x0 {
                        continue;
                    }
                    return String::from_utf8(payload).expect("the frame is not UTF-8");
                }
                // The server asks whether it is still there.
                0x9 => {
                    let pong = masked_frame(&payload, 0xa);
                    self.stream.write_all(&pong).await.unwrap();
                }
                0x8 => panic!("the server closed the connection"),
                _ => {}
            }
        }
    }

    /// Read one frame from the socket.
    async fn read_frame(&mut self) -> (u8, Vec<u8>) {
        let head = self.read_exact(2).await;
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let mut length = (head[1] & 0x7f) as usize;

        if length == 126 {
            let extended = self.read_exact(2).await;
            length = u16::from_be_bytes([extended[0], extended[1]]) as usize;
        } else if length == 127 {
            let extended = self.read_exact(8).await;
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&extended);
            length = u64::from_be_bytes(bytes) as usize;
        }

        let mask = if masked {
            Some(self.read_exact(4).await)
        } else {
            None
        };

        let mut payload = self.read_exact(length).await;
        if let Some(mask) = mask {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }

        (opcode, payload)
    }

    /// Read exactly `count` bytes.
    async fn read_exact(&mut self, count: usize) -> Vec<u8> {
        while self.buffer.len() < count {
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "the server closed the connection");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
        self.buffer.drain(..count).collect()
    }
}

/// A masked frame, as a client must send.
fn masked_frame(payload: &[u8], opcode: u8) -> Vec<u8> {
    let mut frame = vec![0x80 | opcode];
    let length = payload.len();

    if length < 126 {
        frame.push(0x80 | length as u8);
    } else if length <= u16::MAX as usize {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(length as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(length as u64).to_be_bytes());
    }

    // The mask does not need to be secret in a test; it only has to be there.
    let mask = [0x37u8, 0xfa, 0x21, 0x3d];
    frame.extend_from_slice(&mask);
    for (i, byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[i % 4]);
    }
    frame
}

/// A tool that records that it ran.
pub struct RecordingTool {
    pub name: &'static str,
    /// The calls it received.
    pub calls: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Whether the tool needs permission.
    pub requires_approval: bool,
}

#[async_trait]
impl lia::tool::Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "a test tool"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn requires_approval(&self) -> bool {
        self.requires_approval
    }

    async fn run(&self, args: serde_json::Value) -> lia::tool::Result<lia::tool::ToolOutput> {
        self.calls.lock().unwrap().push(args.clone());
        Ok(lia::tool::ToolOutput::ok(format!("ran {}", self.name)))
    }
}

/// A channel that keeps what it was sent, and hands over what it is given.
pub struct FakeChannel {
    /// The messages that were sent out.
    pub sent: Arc<Mutex<Vec<lia::channel::OutboundMessage>>>,
    /// The messages that were received.
    pub received: Arc<Mutex<Vec<lia::channel::InboundMessage>>>,
}

impl FakeChannel {
    pub fn new() -> Self {
        FakeChannel {
            sent: Arc::new(Mutex::new(Vec::new())),
            received: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl lia::channel::Channel for FakeChannel {
    fn name(&self) -> &str {
        "fake"
    }

    async fn run(
        &self,
        inbound: lia::channel::InboundSender,
        _outbound: lia::channel::OutboundSender,
    ) -> anyhow::Result<()> {
        // Hand every message over at once.
        let messages: Vec<lia::channel::InboundMessage> =
            self.received.lock().unwrap().drain(..).collect();
        for message in messages {
            if inbound.send(message).await.is_err() {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn send(&self, message: &lia::channel::OutboundMessage) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push(message.clone());
        Ok(())
    }
}
