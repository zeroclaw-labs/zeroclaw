# Quick Start

The shortest path from zero to talking to the agent.

## Install

Pick one:

**Linux / macOS (one-liner):**

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

**Homebrew (macOS, Linux):**

```bash
brew install zeroclaw
```

**Windows:**

Run `setup.bat` from the latest release, or see [Setup → Windows](../setup/windows.md).

**From source:**

```bash
cargo install --locked --path . # inside a clone
```

## Onboard

```bash
zeroclaw onboard
```

The wizard asks ~9 questions. Minimum inputs:

1. An **LLM provider** (Anthropic, OpenAI, Ollama, OpenRouter, etc.) and its API key or endpoint
2. At least **one channel** — the default `cli` channel works; add Discord, Telegram, Slack, etc. if you want to chat from those platforms

Everything else has safe defaults. Total time: ~2 minutes.

## Talk to it

```bash
zeroclaw agent
```

This drops you into an interactive session using the `cli` channel. Type, get replies. Pass `-m "one-shot message"` for a single non-interactive turn.

For always-on deployment, register the service:

```bash
zeroclaw service install
zeroclaw service start
```

Then use a chat platform channel to reach the agent from Discord, Telegram, or wherever you configured.

## If the wizard's questions annoy you

Run with defaults and skip channel setup:

```bash
zeroclaw onboard --quick --provider ollama --model qwen3.6:35b-a3b
```

Or go all the way and use [YOLO mode](./yolo.md) — one config preset that disables approvals and safety gates. For dev boxes and home labs only.

## Next

- [Multi-model setup](./multi-model-setup.md) — fallback chains, routing rules
- [Setup → Service management](../setup/service.md) — running as a daemon
- [Channels → Overview](../channels/overview.md) — wiring up chat platforms
- [Security → Autonomy levels](../security/autonomy.md) — what the agent is allowed to do
