# ZeroClaw

Personal AI assistant you own, written in Rust.

ZeroClaw is an agent runtime — a single binary you configure and run. It talks to LLM providers (Anthropic, OpenAI, Ollama, and ~20 others), reaches the world through channels (Discord, Telegram, Matrix, email, voice, webhooks, your own CLI), and acts through tools (shell, browser, HTTP, hardware, custom MCP servers). Everything runs on your machine, with your keys, in your workspace.

Read [Philosophy](./philosophy.md) to understand the opinions that shape it.

This site is the documentation. Everything under **Reference → CLI** and **Reference → Config** is generated directly from the code at build time (via `clap` derives and the JSON schema), so it stays in sync with the binary you actually run. Everything else is hand-written user-facing material.

## Where to start

- New to ZeroClaw? → [Quick start](./getting-started/quick-start.md)
- Just want it running fast without safety prompts? → [YOLO mode](./getting-started/yolo.md)
- Installing on a specific platform? → [Linux](./setup/linux.md) · [macOS](./setup/macos.md) · [Windows](./setup/windows.md) · [Docker](./setup/container.md)
- Understanding the architecture? → [Architecture overview](./architecture/overview.md)
- Wiring up a chat platform? → [Channels](./channels/overview.md)
- Pointing it at an LLM? → [Model Providers](./providers/overview.md)
- Adding capabilities? → [Tools](./tools/overview.md)
- Running it in production? → [Operations](./ops/overview.md)
- Writing a workflow? → [SOP](./sop/index.md)
- Building on top of it? → [Developing](./developing/plugin-protocol.md)
- Looking up a flag or config key? → [Reference](./reference/cli.md) · [API rustdoc](./api.md)
- Want to contribute? → [Contributing](./contributing/how-to.md)

## Source

- Upstream: <https://github.com/zeroclaw-labs/zeroclaw>
- Issues, discussions, and RFCs: [GitHub issues](https://github.com/zeroclaw-labs/zeroclaw/issues)
- Real-time chat: Discord (invite link in the repo README)

See [Contributing → Communication](./contributing/communication.md) for the full list of places to reach the project.
