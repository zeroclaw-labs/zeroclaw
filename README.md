<p align="center">
  <img src="zeroclaw.png" alt="ClawPilot" width="200" />
</p>

<h1 align="center">ClawPilot 🦀</h1>

<p align="center">
  <strong>Zero overhead. Zero compromise. 100% Rust. 100% model-agnostic.</strong><br>
  Fast, lightweight, and secure AI runtime infrastructure for local-first, control-oriented workflows.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
</p>

> **Repo name is `clawpilot`; the CLI command remains `zeroclaw` for compatibility.**

```text
~3.4MB binary · <10ms startup · 1,017 tests · 22+ providers · trait-based architecture · pluggable What is ClawPilot?

ClawPilot is a lean, Rust-native AI runtime and tool orchestrator for people who want:
	•	small binaries
	•	fast startup
	•	strict sandboxing
	•	workspace-scoped execution
	•	provider flexibility
	•	low-resource deployment
	•	no dependency on a single AI vendor

It is designed for local-first, supervised, and control-oriented AI operations rather than heavyweight “assistant platform” convenience layers.

ClawPilot runs as a compact binary, supports multiple model providers, exposes tool and channel abstractions through traits, and can be deployed on anything from a low-cost Linux board to a full desktop environment.

Why teams pick ClawPilot
	•	Lean by default — small Rust binary, fast startup, low memory footprint
	•	Secure by design — pairing, sandboxing, explicit allowlists, workspace scoping
	•	Fully swappable — providers, channels, tools, memory, runtime, tunnel, and identity systems are all modular
	•	No lock-in — supports OpenAI-compatible endpoints plus a broad provider surface
	•	Practical for constrained hardware — suitable for edge devices and low-cost Linux systems

Highlights
	•	🦀 Rust-native runtime with a small release binary
	•	⚡ Fast startup and low-overhead execution
	•	🌍 Portable deployment across ARM, x86, and RISC-V targets
	•	🔌 Model-agnostic provider system
	•	🧠 Hybrid memory engine with FTS5 + vector similarity
	•	🛡️ Security guardrails with pairing, scoping, sandboxing, and allowlists
	•	🧩 Trait-based architecture for extensibility
	•	🤖 Channels, tools, memory, identity, and runtime adapters are all pluggable

ClawPilot update highlights

Recent ClawPilot-focused improvements include:
	•	Unified runtime behavior with explicit fail-fast errors for unsupported runtime adapters
	•	Expanded integrations surface with 50+ integrations and 22+ model providers
	•	Hybrid memory engine improvements with FTS5 + vector similarity and weighted merges
	•	Hardware-oriented peripheral workflow support for STM32, ESP32, and Raspberry Pi
	•	Clearer Linux operator workflows and more practical runtime guardrails

How ClawPilot differs from OpenClaw
	•	OpenClaw is a broader personal-assistant/product stack with chat channel integrations, gateway endpoints, and convenience-focused setup UX
	•	ClawPilot is a lean runtime and orchestrator focused on supervised execution, explicit control, and lower resource usage
	•	In this fork, the emphasis is on:
	•	Linux operator workflows
	•	OpenRouter clarity
	•	strong sandboxing and allowlists
	•	practical runtime guardrails over convenience defaults

In short:
	•	OpenClaw aims for a broader assistant experience
	•	ClawPilot aims for a lighter, more controllable runtime core

Benchmark snapshot

The table below reflects a local benchmark snapshot and should be treated as a directional comparison, not a universal benchmark across all environments.

Local machine quick benchmark: macOS arm64, Feb 2026, normalized for 0.8GHz edge hardware.
