<p align="center">
  <img src="https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/docs/assets/zeroclaw-banner.png" alt="ZeroClaw" width="600" />
</p>

<h1 align="center">🦀 ZeroClaw — Personal AI Assistant</h1>

<p align="center">
  <strong>You own the agent. You own the data. You own the machine it runs on.</strong>
</p>

<p align="center">
  <a href="https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/zeroclaw-labs/zeroclaw/ci.yml?branch=master&label=build" alt="Build Status" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/releases/latest"><img src="https://img.shields.io/github/v/release/zeroclaw-labs/zeroclaw?label=release" alt="Latest release" /></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-edition%202024-orange?logo=rust" alt="Rust Edition 2024" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contributors" /></a>
  <a href="https://discord.com/invite/wDshRVqRjx"><img src="https://img.shields.io/badge/Discord-join-5865F2?style=flat&logo=discord&logoColor=white" alt="Discord" /></a>
</p>

<p align="center">
  <a href="docs/book/src/introduction.md">Docs</a> ·
  <a href="docs/book/src/philosophy.md">Philosophy</a> ·
  <a href="docs/book/src/getting-started/quick-start.md">Quick start</a> ·
  <a href="docs/book/src/architecture/overview.md">Architecture</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

---

ZeroClaw is an agent runtime — a single Rust binary you configure and run. It talks to LLM providers (Anthropic, OpenAI, Ollama, and ~20 others), reaches the world through 30+ channels (Discord, Telegram, Matrix, email, voice, webhooks, your own CLI), and acts through tools (shell, browser, HTTP, hardware, custom MCP servers). Everything runs on your machine, with your keys, in your workspace.

Read the [Philosophy](docs/book/src/philosophy.md) for the four opinions that shape it.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

Or clone and run:

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

The installer asks whether you want a prebuilt binary (fast, ~seconds) or a source build (slower, customisable). Both end the same way — `zeroclaw onboard` kicks off automatically.

Flags:

```
./install.sh --prebuilt              # always prebuilt; don't ask
./install.sh --source                # always build from source
./install.sh --minimal               # kernel only (~6.6 MB)
./install.sh --source --features agent-runtime,channel-discord  # custom feature set
./install.sh --skip-onboard          # install only, run `zeroclaw onboard` later
./install.sh --list-features         # print available feature flags
```

Platform-specific notes: [Linux](docs/book/src/setup/linux.md) · [macOS](docs/book/src/setup/macos.md) · [Windows](docs/book/src/setup/windows.md) · [Docker](docs/book/src/setup/container.md)

## Quick start

```bash
zeroclaw onboard                  # wizard: picks a provider, wires channels
zeroclaw agent                    # interactive chat in the terminal
zeroclaw service install          # register as systemd/launchctl/Windows Service
zeroclaw service start            # run it always-on in the background
```

Full walkthrough: [Quick start](docs/book/src/getting-started/quick-start.md) — or skip the safety gates with [YOLO mode](docs/book/src/getting-started/yolo.md) for dev boxes.

## What ZeroClaw does

- **Multi-channel** — one agent answering you across [every channel you configure](docs/book/src/channels/overview.md). Inbound messages from Discord, Telegram, Matrix, email, webhooks, CLI — all delivered to the same agent loop.
- **Provider-agnostic** — [model providers](docs/book/src/providers/overview.md) are pluggable. Configure Anthropic, OpenAI, local Ollama, or any OpenAI-compatible endpoint. [Fallback chains and routing](docs/book/src/providers/fallback-and-routing.md) keep the agent running when a provider flakes.
- **Security-first, with escape hatches** — default autonomy is `supervised`: medium-risk ops require approval, high-risk blocked. Workspace boundaries, command policy, OS-level sandboxes (Landlock / Bubblewrap / Seatbelt / Docker), and cryptographic [tool receipts](docs/book/src/security/tool-receipts.md) on every action. [YOLO mode](docs/book/src/getting-started/yolo.md) exists for trusted dev environments.
- **Hardware-capable** — GPIO / I2C / SPI / USB on Raspberry Pi, STM32, Arduino, and ESP32 via the `Peripheral` trait. See [Hardware](docs/book/src/hardware/index.md).
- **Gateway + dashboard** — HTTP / WebSocket gateway for clients, with a web dashboard for chat, memory browsing, config editing, cron management, and tool inspection.
- **SOP engine** — event-triggered [Standard Operating Procedures](docs/book/src/sop/index.md) (MQTT / webhook / cron / peripheral) with approval gates and resumable runs.
- **ACP** — IDE / editor integration via [Agent Client Protocol](docs/book/src/channels/acp.md) (JSON-RPC 2.0 over stdio).

## Configuration

One TOML file at `~/.zeroclaw/config.toml`. Pointers:

- [Provider configuration](docs/book/src/providers/configuration.md) — the universal `[providers.models.<name>]` schema
- [Channels overview](docs/book/src/channels/overview.md) — per-channel `[channels.<name>]` blocks
- [Security overview](docs/book/src/security/overview.md) — autonomy, sandboxing, tool receipts
- [Full config reference](docs/book/src/reference/config.md) — generated from the live schema; every key documented

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│            channels       gateway        ACP                 │
│          (30+ adapters)   (REST/WS)    (JSON-RPC)            │
│                        ↓                                     │
│                   ZeroClaw runtime                           │
│         ┌──────────┬──────────┬──────────┐                   │
│         │  agent   │ security │   SOP    │                   │
│         │   loop   │  policy  │  engine  │                   │
│         └──────────┴──────────┴──────────┘                   │
│              ↓          ↓           ↓                        │
│          providers    tools      memory                      │
│         (Anthropic,  (shell,    (SQLite,                     │
│          OpenAI,     browser,    embeddings)                 │
│          Ollama,     HTTP,                                   │
│          ~20 more)   hardware)                               │
└──────────────────────────────────────────────────────────────┘
```

Full detail with Mermaid diagrams: [Architecture overview](docs/book/src/architecture/overview.md) · [Request lifecycle](docs/book/src/architecture/request-lifecycle.md) · [Crates](docs/book/src/architecture/crates.md).

## Contributing

Start with [how to contribute](docs/book/src/contributing/how-to.md). Larger changes go through the [RFC process](docs/book/src/contributing/rfcs.md). Real-time chat lives on [Discord](https://discord.com/invite/wDshRVqRjx) (the best way to reach the team); durable work tracking is on [GitHub issues](https://github.com/zeroclaw-labs/zeroclaw/issues).

Good places to start:

- New channel → `crates/zeroclaw-channels/`
- New provider → `crates/zeroclaw-providers/`
- New tool → `crates/zeroclaw-tools/`
- Hardware support → `crates/zeroclaw-hardware/`
- Docs → `docs/book/src/`

AI-assisted PRs are welcome; see [Contribution culture (RFC #5615)](https://github.com/zeroclaw-labs/zeroclaw/issues/5615) for the co-authorship norms.

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## Security

Do not file public issues for security vulnerabilities. Email `security@zeroclaw.dev`. See [SECURITY.md](SECURITY.md) for the full policy.

## Official repository & impersonation notice

This is the only official ZeroClaw repository:

> <https://github.com/zeroclaw-labs/zeroclaw>

Any other repository, organization, domain, or package claiming to be "ZeroClaw" or implying affiliation with ZeroClaw Labs is **unauthorized and not affiliated with this project**.

## License

Dual-licensed: [MIT](LICENSE-MIT) OR [Apache 2.0](LICENSE-APACHE). You may choose either. Contributors automatically grant rights under both — see [CLA](docs/book/src/contributing/cla.md). The **ZeroClaw** name and logo are trademarks of ZeroClaw Labs.

## Credits

Built and maintained by the community — original creator [@theonlyhennygod](https://github.com/theonlyhennygod); project lead [@JordanTheJet](https://github.com/JordanTheJet). Full maintainer list in [Communication](docs/book/src/contributing/communication.md).

Thanks to the communities that incubated early work: **Harvard University**, **MIT**, **Sundai Club**, and every contributor pushing it forward.

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>

<p align="center">
  <a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors">
    <img src="https://contrib.rocks/image?repo=zeroclaw-labs/zeroclaw" alt="ZeroClaw contributors" />
  </a>
</p>
