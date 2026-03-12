# LightWave Augusta

Local AI agent runtime for macOS. Forked from [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw).

## Quick Start

```bash
cargo build --release
./target/release/augusta agent
```

## Architecture

- **Agent Loop**: `src/agent/` — LLM provider routing + tool call execution
- **Channels**: `src/channels/` — CLI (stdin/stdout), Orchestrator (Redis Streams)
- **Tools**: `src/tools/` — Shell, file ops, browser, memory, git, screenshots
- **Providers**: `src/providers/` — Anthropic, OpenAI, Ollama, OpenRouter
- **Memory**: `src/memory/` — SQLite hybrid search (vector + FTS5)
- **Security**: `src/security/` — Filesystem isolation, allowlists, secret encryption
- **macOS Desktop**: `crates/lightwave-macos/` — Native window/input/screen/app control

## License

MIT OR Apache-2.0
