# Layout

```
src/
├── agent/       the loop, session storage, context compaction
├── provider/    neutral message types + Anthropic, OpenAI, Ollama and llama.cpp adapters
├── tool/        the built-in tools and the approval policy
├── mcp/         the MCP client, and the file server it starts for itself
├── gateway/     the daemon: HTTP and WebSocket
├── channel/     messaging platform adapters
├── image.rs     the pictures the assistant is sent
├── schedule/    the work the assistant promises to do later
├── voice/       the microphone, the words, and the speaker
├── tui/         the terminal interface
├── cli.rs       command-line entry point
├── skill.rs     skills, and the tool that loads them
├── memory.rs    what the assistant keeps between conversations
└── config.rs    configuration
web/
├── index.html   the markup of the interface
├── css/         the stylesheets, one per region of the page
└── js/          the behaviour, one file per part of the page
```

The stylesheet and script files are served under `/assets/`, and all of it
is held inside the binary by `include_str!`.
