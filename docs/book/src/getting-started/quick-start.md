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

## Quickstart

```bash
zeroclaw quickstart
```

`zeroclaw quickstart` writes a working config with one provider and one agent in a single shot. Minimum inputs:

1. An **LLM provider** (Anthropic, OpenAI, Ollama, OpenRouter, etc.) and its API key or endpoint
2. An **agent alias** — defaults to a sanitized provider name

Channels are configured separately. The default `cli` channel works out of the box; to add Discord, Telegram, Slack, etc., use `zeroclaw config set channels.<name>.<field>=<value>` or follow the per-channel guide under [Channels → Overview](../channels/overview.md).

Everything else has safe defaults. Total time: ~1 minute.

## Talk to it

```bash
zeroclaw agent -a <alias>
```

`<alias>` matches your `[agents.<alias>]` config entry — required, no default. This drops you into an interactive session using the `cli` channel. Pass `-m "one-shot message"` for a single non-interactive turn.

For always-on deployment, register the service:

```bash
zeroclaw service install
zeroclaw service start
```

Then use a chat platform channel to reach the agent from Discord, Telegram, or wherever you configured.

## Skip the prompts

Run non-interactively by passing all required flags:

```bash
zeroclaw quickstart --model-provider ollama --model qwen3.6:35b-a3b
```

Add `--api-key <key>` for hosted providers and `--agent <alias>` to override the default agent name. Or go all the way and use [YOLO mode](./yolo.md) — one config preset that disables approvals and safety gates. For dev boxes and home labs only.

## Next

- [Multi-model setup](./multi-model-setup.md) — multi-agent dispatch, hint-based routes
- [Setup → Service management](../setup/service.md) — running as a daemon
- [Channels → Overview](../channels/overview.md) — wiring up chat platforms
- [Security → Autonomy levels](../security/autonomy.md) — what the agent is allowed to do
