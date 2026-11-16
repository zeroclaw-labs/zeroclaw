<!--
  Canonical Quickstart step list for the CLI. It drives the same `BuilderSubmission` and produce
  identical config. Only the launch path and chrome differ. Edit the steps
  here once; every surface page picks up the change via {{#include}}.
-->

Every surface walks the same checklist and writes the same config. Required
steps must be satisfied before the agent can be created; optional steps can be
skipped.

| Step | Required | What it sets |
|---|---|---|
| **Model provider** | yes | Provider family (Anthropic, OpenAI, Ollama, OpenRouter, …), its API key or endpoint, and the model. |
| **Risk profile** | yes | Autonomy and sandbox posture. Pick a preset or reuse an existing `[risk_profiles.<alias>]`. |
| **Memory** | yes | Memory backend (`tachi`, `markdown`, or `none`). |
| **Channels** | optional | Live surfaces (Telegram bridge, webhook ingress, Email/Discord tools). The built-in `cli` channel always works; add others here or later. |
| **Peer groups** | optional | Multi-agent peer membership for the channels you configured. |
| **Agent** | yes | Agent alias, system prompt, and any personality files. |

> The runtime profile is set automatically. Quickstart installs the
> `unbounded` preset for the new agent. Tune budgets and timeouts afterward by
> editing `[runtime_profiles.<alias>]` (see
> [Reference → Config](../reference/config.md)).
