# Philosophy

ZeroClaw is built on four opinions, in priority order.

## You own it

The binary runs on your machine, your VPS, or your SBC. Your API keys live in your config file. Your conversation history lives in your database. No telemetry, no cloud tenancy, no license server. If you pull the power cord, the agent stops, and nothing else breaks.

This is the foundational constraint. Every other decision below falls out of it.

## Security-first, with escape hatches

Local-first doesn't mean consequence-free. An agent that can execute shell commands, call HTTP endpoints, and write files is a privileged process. The default autonomy level is `supervised`: medium-risk operations require approval, high-risk operations are blocked.

The runtime ships with:

- Workspace boundaries (the agent can only touch paths inside its configured workspace)
- Command allow/deny lists
- Shell-policy validation
- OS-level sandboxes (Docker, Firejail, Bubblewrap, Landlock on Linux; Seatbelt on macOS)
- Tool receipts: a cryptographically-linked audit log of every tool call
- Emergency stop (`zeroclaw estop`) and OTP-gated actions

For developers and home-lab users who understand the trade-offs, there's [YOLO mode](./getting-started/yolo.md): one config preset that disables the guardrails. It's loud, logged, and obviously named. Not the default.

## Minimal: in binary size, dependencies, and surface area

ZeroClaw is written in Rust and optimised for a small binary and fast startup. The microkernel split ([RFC #5574](https://github.com/zeroclaw-labs/zeroclaw/issues/5574)) factors functionality behind feature flags so you only ship what you use: the foundation builds with `--no-default-features`, and channels, hardware, and the gateway are opt-in. A typical release build lands around 26 MiB; a minimal feature set trims it further.

The same discipline applies to the agent's prompt surface. Tool descriptions are [Fluent](https://projectfluent.org/)-localised and terse. There are no hidden system prompts injecting personality. The model sees what you configure.

## Provider-agnostic

The agent's brain is pluggable. Anthropic, OpenAI, Ollama, Bedrock, Gemini, Azure, OpenRouter, and any OpenAI-compatible endpoint (Groq, Mistral, xAI, and ~20 others) work out of the box. Per-agent dispatch and hint-based model routes let you run reasoning-heavy tasks on one model and cheap chat on another.

This is deliberate. We have opinions about quality but not about vendors. If a better model ships tomorrow under a different banner, the config is a one-line change.

## What this isn't

- **Not a SaaS.** There's no hosted version, no account system, no billing.
- **Not only a chat UI.** It ships chat front-ends, the [zerocode](./zerocode/overview.md) terminal interface, the web dashboard, and chat-platform channels, but those sit on top of an agent runtime. The runtime is the product; the chat surfaces are how you reach it, alongside the CLI, the REST gateway, and the ACP JSON-RPC interface.
- **Not a framework.** You don't build apps on top of ZeroClaw. You configure it and connect channels.
- **Not a toy.** Production deployments run 24/7 on homelab SBCs, VPSes, and cloud VMs. The `zeroclaw service` subcommand manages systemd / launchctl / Windows Service registration out of the box.

## How decisions get made

Substantive changes go through the RFC process: see [Contributing → RFCs](./contributing/rfcs.md). An RFC labelled `status:accepted` is ratified and binding even while its implementation is still open; the discussion thread stays the living record until the work lands.

The ratified foundational RFCs, the maturity framework this project is built on, live in the book as the [Foundations](./foundations/README.md) section, versioned alongside the code. Start there for the canonical, always-current set rather than a list duplicated here.
