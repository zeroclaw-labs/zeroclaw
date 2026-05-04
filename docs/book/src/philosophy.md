# Philosophy

ZeroClaw is built on four opinions, in priority order.

## 1. You own it

The binary runs on your machine, your VPS, or your SBC. Your API keys live in your config file. Your conversation history lives in your database. No telemetry, no cloud tenancy, no license server. If you pull the power cord, the agent stops — and nothing else breaks.

This is the foundational constraint. Every other decision below falls out of it.

## 2. Security-first, with escape hatches

Local-first doesn't mean consequence-free. An agent that can execute shell commands, call HTTP endpoints, and write files is a privileged process. The default autonomy level is `supervised` — medium-risk operations require approval, high-risk operations are blocked.

The runtime ships with:

- Workspace boundaries (the agent can only touch paths inside its configured workspace)
- Command allow/deny lists
- Shell-policy validation
- OS-level sandboxes (Docker, Firejail, Bubblewrap, Landlock on Linux; Seatbelt on macOS)
- Tool receipts — a cryptographically-linked audit log of every tool call
- Emergency stop (`zeroclaw estop`) and OTP-gated actions

For developers and home-lab users who understand the trade-offs, there's [YOLO mode](./getting-started/yolo.md) — one config preset that disables the guardrails. It's loud, logged, and obviously named. Not the default.

## 3. Minimal — in binary size, dependencies, and surface area

ZeroClaw is written in Rust and optimised for a small binary and fast startup. A microkernel roadmap ([RFC #5574](https://github.com/zeroclaw-labs/zeroclaw/issues/5574)) is actively splitting functionality behind feature flags so you only ship what you use. A release build of the core runtime fits in tens of megabytes; adding channel integrations or hardware support is opt-in.

The same discipline applies to the agent's prompt surface. Tool descriptions are [Fluent](https://projectfluent.org/)-localised and terse. There are no hidden system prompts injecting personality. The model sees what you configure.

## 4. Provider-agnostic

The agent's brain is pluggable. Anthropic, OpenAI, Ollama, Bedrock, Gemini, Azure, OpenRouter, and any OpenAI-compatible endpoint (Groq, Mistral, xAI, and ~20 others) work out of the box. Fallback chains and routing rules let you run reasoning-heavy tasks on one model and cheap chat on another, with automatic failover.

This is deliberate. We have opinions about quality but not about vendors. If a better model ships tomorrow under a different banner, the config is a one-line change.

## What this isn't

- **Not a SaaS.** There's no hosted version, no account system, no billing.
- **Not a chat UI.** It's an agent runtime. You bring the front end — a CLI, a chat platform channel, the REST gateway, or the ACP JSON-RPC interface.
- **Not a framework.** You don't build apps on top of ZeroClaw. You configure it and connect channels.
- **Not a toy.** Production deployments run 24/7 on homelab SBCs, VPSes, and cloud VMs. The `zeroclaw service` subcommand manages systemd / launchctl / Windows Service registration out of the box.

## How decisions get made

Substantive changes go through the RFC process — see [Contributing → RFCs](./contributing/rfcs.md). Accepted RFCs are canonical. Open RFCs are discussion documents; they are the primary reference for what's coming next and why.

Ratified foundational RFCs:

- **[#5574](https://github.com/zeroclaw-labs/zeroclaw/issues/5574)** — Microkernel transition (v0.7.0 → v1.0.0). Crate splits, feature-flag taxonomy.
- **[#5576]((https://github.com/zeroclaw-labs/zeroclaw/issues/5576)** — Documentation standards and knowledge architecture.
- **[#5577]((https://github.com/zeroclaw-labs/zeroclaw/issues/5577)** — Project governance: core-team structure, two-thirds-majority voting.
- **[#5579]((https://github.com/zeroclaw-labs/zeroclaw/issues/5579)** — Engineering infrastructure: CI pipelines, release automation.
- **[#5615]((https://github.com/zeroclaw-labs/zeroclaw/issues/5615)** — Contribution culture: human/AI co-authorship norms.
- **[#5653]((https://github.com/zeroclaw-labs/zeroclaw/issues/5653)** — Zero Compromise: error handling, dead-code policy, release-readiness.
