# FND-001: Intentional Architecture — ZeroClaw Microkernel Transition
### Starting v0.7.0 · Type: Architecture · Rev. 3

> **Canonical reference** · Ratified by the team · Rev. 3
> Discussion thread and full revision history: [#5574](https://github.com/zeroclaw-labs/zeroclaw/issues/5574)


---

> **A note to the team before you read this.**
>
> This document was written to help us move from a codebase that grew reactively into one that is built with intention. If some of the concepts here are new to you, that is not a problem — it means this document is doing its job. Every senior engineer you will ever work with has learned these lessons the hard way, on a codebase that got too big to understand. We have the rare opportunity to recognize the pattern early and course-correct before it becomes painful. This is a good thing. Take your time with it.

---

## Table of Contents

1. [A Development Philosophy: Vision First](#1-a-development-philosophy-vision-first)
2. [The Vision — What ZeroClaw Is](#2-the-vision--what-zeroclaw-is)
3. [Honest Assessment: Where We Are Today](#3-honest-assessment-where-we-are-today)
4. [The Target Architecture](#4-the-target-architecture)
   - [4.4.1 Versioning Policy](#441-versioning-policy)
   - [4.4.2 Release Artifacts](#442-release-artifacts)
5. [Standards We Should Adopt](#5-standards-we-should-adopt)
6. [Phased Roadmap: v0.7.0 → v1.0.0](#6-phased-roadmap-v070--v100)
7. [Code and Complexity Metrics](#7-code-and-complexity-metrics)
8. [What This Means for Contributors](#8-what-this-means-for-contributors)

---

## Revision History

| Rev | Date | Summary |
|---|---|---|
| 1 | 2026-04-09 | Initial draft |
| 2 | 2026-04-09 | Added §4.4.1 Versioning Policy (unified workspace inheritance, stability tiers, product-level breaking change definition); added §4.4.2 Release Artifacts (feature flag fate, canonical release binary profile, release artifact matrix); added Discussion Questions for versioning strategy and observability defaults |
| 3 | 2026-04-10 | Terminology correction per implementation feedback from PR #5559: "kernel" → "runtime" for the agent orchestration layer throughout; "kernel" now refers specifically to the irreducible foundation (`--no-default-features` build); §4.1 updated to describe the explicit two-layer architecture (foundation + runtime); §4.2–§4.3 dependency diagram and component map updated to show `zeroclaw-runtime`; Phase 2 renamed from "The Kernel" to "The Runtime"; binary size targets reframed as aspirational north stars with measured progress tracking rather than hard gates; §7 updated with actual Phase 1 measurement (6.6 MB foundation build) and explicit note that architectural decomposition enables optimization but optimization is a dedicated second pass |

---

## 1. A Development Philosophy: Vision First

Every decision we make in software — what to build, how to build it, what to skip — should flow downward from a hierarchy of intent:

```
Vision
  └── Architecture
        └── Design
              └── Implementation
                    └── Testing
                          └── Documentation
                                └── Release
```

This is not a waterfall process. It is a **decision hierarchy**. It means that when you are writing a function, you should be able to trace a straight line upward: this function exists because of this design decision, which exists because of this architectural choice, which exists because of this vision. If you cannot draw that line, the code probably should not exist.

**What each layer means in practice:**

| Layer | The Question It Answers | What Goes Wrong Without It |
|---|---|---|
| **Vision** | *Why does this project exist? Who is it for? What does success look like?* | You build things nobody needs, or contradict yourself across releases |
| **Architecture** | *What are the structural decisions that make the vision possible?* | You end up with a "Big Ball of Mud" — code that works but cannot be changed without breaking something else |
| **Design** | *How do the components relate? What are the interfaces between them?* | You get tight coupling — components that know too much about each other's internals |
| **Implementation** | *How do we build this specific component?* | Bugs, performance issues, security holes |
| **Testing** | *Does the implementation match the design? Does the design serve the architecture?* | You ship broken things and don't know why |
| **Documentation** | *How do we transfer this knowledge to the next person?* | Every contributor has to rediscover everything from scratch |
| **Release** | *How do we get this to users safely and sustainably?* | Users get broken or confusing software |

### The Problem With Skipping the Top

ZeroClaw was bootstrapped by AI tools working from OpenClaw's TypeScript codebase. AI code generation works at the **Implementation** layer. It writes functions, structs, and modules that do things. It does not set Vision. It does not make Architecture decisions. It does not define Design contracts.

The result is a codebase that is impressively functional but architecturally accidental. The code does what it needs to do today, but it was not designed — it accumulated. This pattern has a name in our industry: **the Big Ball of Mud**. It is the most common architecture in software, not because anyone chose it, but because it is what you get when you skip the top of the hierarchy.

This RFC is our chance to fix that — not by throwing away what works, but by growing an intentional architecture around it using a technique called the **Strangler Fig Pattern**: we build the new structure around the edges of the old one, migrating inward over time, until the old structure is gone. No "big bang" rewrite. No throwing away working code. Just steady, intentional improvement.

---

## 2. The Vision — What ZeroClaw Is

Before we talk about architecture, we need to be precise about what we are building. This is the Vision layer. Everything that follows must serve this.

> **ZeroClaw is a personal AI assistant runtime that any person can run on any hardware — from a $10 embedded board to a cloud server — with zero configuration overhead, zero external service requirements, and zero compromise on capability or security.**

Breaking that down into concrete commitments:

**Zero overhead.** The core agent starts in milliseconds and uses less memory than a browser tab. This is not a marketing claim — it is an architectural constraint. Every decision we make must be tested against it.

**Zero external requirements.** A user who downloads ZeroClaw and has an LLM provider configured should have a working, useful AI assistant without installing anything else. Channels, dashboards, and integrations are things you add when you want them — not things you need before it works.

**Zero compromise.** Lean does not mean weak. ZeroClaw must have a serious security model, real observability, and genuine extensibility. The tension between "small binary" and "full capability" is resolved through composition: a small core, extended by components you choose.

**For every skill level.** A student on a $10 Raspberry Pi and a team running a production deployment should both feel like ZeroClaw was designed for them. This means the default experience must be simple, and the advanced experience must be powerful — not two different products.

**User-owned.** Your data, your hardware, your configuration. ZeroClaw does not require an account, does not phone home, and does not lock you into a platform.

---

## 3. Honest Assessment: Where We Are Today

This section is not criticism of anyone's work. It is a diagnosis, and you cannot fix what you do not name.

### 3.1 The Structural Problem

The entire ZeroClaw codebase currently lives in a single Rust crate. This means:

- A Telegram channel and the core agent loop are compiled from the same source tree whether you use Telegram or not
- The web dashboard (a full React application) is embedded in the binary using `rust-embed`, making every binary include the web UI even for users who only ever use the CLI
- The gateway HTTP server contains webhook handlers for WhatsApp, WATI, Linq, Nextcloud Talk, and Gmail — meaning specific channel integrations are baked into the web server
- Every one of the 70+ tools is compiled into the binary, regardless of which tools a user will ever call
- The only mechanism for excluding code is a Cargo feature flag, which requires users to have a Rust development environment and recompile from source

**The consequence for users:** The stated goal is a lean binary for $10 hardware. But the binary ships with code for 27 messaging channels, 70+ tools, a full web server, a React application, and integrations with Jira, Notion, Google Workspace, LinkedIn, and more — most of which any given user will never touch.

**The consequence for contributors:** When a file is 9,500 lines long, it is not possible to understand it. When every feature is in one crate, touching anything risks breaking everything.

### 3.2 The Evidence

These are measured facts from the current codebase, not estimates:

| File | Lines | What It Does | What It Should Do |
|---|---|---|---|
| `src/agent/loop_.rs` | ~9,500 | Tool call parsing, streaming, history, cost tracking, model routing, memory, credential scrubbing, context building | Orchestrate a single agent turn |
| `src/gateway/mod.rs` | ~2,260 | Web server + React app server + WhatsApp webhooks + WATI webhooks + Linq webhooks + Nextcloud webhooks + Gmail webhooks + pairing + rate limiting + WebAuthn | Serve the web dashboard API |
| `src/providers/mod.rs` | ~3,750 | Factory + 40+ provider implementations + OAuth flows + credential resolution + error scrubbing | Route to a provider |
| `src/tools/mod.rs` | `all_tools_with_runtime()` at L387–L1066 | Instantiate all 70+ tools unconditionally | Register the tools the user configured |

**A 9,500-line file is not a module. It is a monolith that happens to have a `.rs` extension.**

### 3.3 What Is Already Good

This diagnosis should not obscure what is genuinely well-designed:

- **The trait layer is excellent.** `Provider`, `Channel`, `Tool`, `Memory`, `Observer`, `RuntimeAdapter`, and `Peripheral` are clean, well-documented Rust traits. These are the right seams. The problem is they do not correspond to crate boundaries, so the compiler cannot enforce the layering.
- **The WASM plugin system is partially built.** `PluginHost`, `WasmTool`, `WasmChannel`, `PluginManifest`, and Ed25519 signature verification all exist in `src/plugins/`. The execution bridge is a stub, but the structure is correct.
- **The observability system is mature.** OpenTelemetry, Prometheus, and DORA metrics are all implemented against a clean `Observer` trait. This is production-quality work.
- **The security model is thoughtful.** Pairing codes, autonomy levels, sandboxing, and policy enforcement show real design intent.

We are not rewriting ZeroClaw. We are giving its existing good ideas a structure they can grow in.

---

## 4. The Target Architecture

### 4.1 The Microkernel Model

A microkernel architecture separates a minimal, stable core from optional subsystems that extend it. In operating systems, the classic example is a kernel that only handles memory and scheduling, with everything else — filesystems, device drivers, network stacks — running as separate processes that communicate through a well-defined interface.

For an AI agent runtime, the mapping reveals **two distinct internal layers** that the OS analogy conflates:

| OS Microkernel Concept | ZeroClaw Equivalent |
|---|---|
| Kernel | **Foundation layer** — API traits, config, providers, memory backends, infra, tool-call parser. The irreducible core: builds with `--no-default-features`. Can exchange messages with an LLM and store memory. Nothing more. |
| Init / runtime system | **Agent runtime layer** — Orchestration loop, security policy enforcement, plugin host, core tools, IPC API. The `zeroclaw-runtime` crate, gated by the `agent-runtime` feature. This is what makes ZeroClaw an *agent*, not just a library. |
| IPC | Local socket / IPC API between the runtime and external components |
| Device drivers | Channel plugins (Telegram, Discord, etc.) |
| Filesystem drivers | Memory backend plugins (SQLite, Markdown) |
| User processes | Gateway binary, Tauri desktop app |

The distinction matters: the **foundation** is the minimum that must exist for any ZeroClaw binary to function. The **runtime** is the minimum that must exist for it to function *as an agent*. Everything else is composed in.

This two-layer split was identified during the Phase 1 workspace decomposition (PR #5559) and is reflected in the crate naming: `zeroclaw-runtime` (the crate) is gated by `agent-runtime` (the feature). The earlier revisions of this RFC used "kernel" loosely to refer to what is now correctly named the runtime layer. This revision corrects that terminology throughout.

### 4.2 The Dependency Rule

The most important architectural rule in this design — the one that, if broken, collapses the whole structure — is this:

> **Dependencies flow inward. The runtime knows nothing about the plugins. Plugins know about the API. Nothing knows about everything.**

```
    zeroclaw-api          ← defines all traits (Provider, Channel, Tool, ...)
         ▲                  no implementations, no heavy dependencies
         │ depends on
  foundation crates       ← zeroclaw-config, zeroclaw-providers, zeroclaw-memory,
         ▲                  zeroclaw-infra, zeroclaw-tool-call-parser
         │ depends on        all depend on zeroclaw-api; no cross-dependencies
    zeroclaw-runtime      ← implements the agent loop (agent-runtime feature)
         ▲                  depends on zeroclaw-api + foundation crates
         │ depends on        knows nothing about specific channels or tools
  plugin crates           ← zeroclaw-channel-discord, zeroclaw-tools-web, ...
         ▲                  depend on zeroclaw-api (not the runtime)
         │ depends on
  zeroclaw binary         ← thin wiring layer
                             reads config, registers plugins, starts runtime
```

If `zeroclaw-runtime` ever imports `TelegramChannel`, the architecture has been violated. The compiler will enforce this once crate boundaries are drawn.

### 4.3 Component Map

```
┌─────────────────────────────────────────────────────────────────────┐
│                    zeroclaw (binary crate)                          │
│  Reads config → registers only configured components → starts       │
│                                                                     │
│   ┌──────────────────────────────────────────────────────────────┐  │
│   │              zeroclaw-runtime  (agent-runtime feature)       │  │
│   │                                                              │  │
│   │  Agent Loop · CLI Channel · Security Policy                  │  │
│   │  Plugin Host · Local IPC API                                 │  │
│   │  Core Tools: shell, file, git, memory recall/store           │  │
│   │                                                              │  │
│   │  ┌──────────────────────────────────────────────────────┐   │  │
│   │  │   Foundation  (--no-default-features)                │   │  │
│   │  │                                                      │   │  │
│   │  │  zeroclaw-api · zeroclaw-config · zeroclaw-infra     │   │  │
│   │  │  zeroclaw-providers · zeroclaw-memory                │   │  │
│   │  │  zeroclaw-tool-call-parser                           │   │  │
│   │  │                                                      │   │  │
│   │  │  Vision target: <5 MB RAM at runtime                 │   │  │
│   │  └──────────────────────────────────────────────────────┘   │  │
│   └──────────────────────────────────────────────────────────────┘  │
│                              ▲                                      │
│                   zeroclaw-api (traits only)                        │
│                              ▲                                      │
│   ┌──────────────┐  ┌────────┴────────┐  ┌─────────────────────┐   │
│   │  zeroclaw-gw │  │  Channel plugins│  │   Tool plugins      │   │
│   │  (opt-in     │  │                 │  │                     │   │
│   │   binary)    │  │  channel-discord│  │  tools-web          │   │
│   │              │  │  channel-slack  │  │  tools-integrations │   │
│   │  HTTP/WS/SSE │  │  channel-tg     │  │  tools-hardware     │   │
│   │  Web UI      │  │  channel-email  │  │  tools-mcp          │   │
│   │  REST API    │  │  ...            │  │  ...                │   │
│   └──────┬───────┘  └─────────────────┘  └─────────────────────┘   │
│          │                                                          │
│          ▼                                                          │
│   ┌─────────────────┐                                               │
│   │ zeroclaw-desktop│   ← Tauri app (already exists in apps/tauri)  │
│   │ System tray app │     bundles zeroclaw-gw as a sidecar          │
│   │ Native GUI      │                                               │
│   └─────────────────┘                                               │
└─────────────────────────────────────────────────────────────────────┘
```

### 4.4 The Distribution Model

The architecture enables a clean distribution story that requires no Rust toolchain from end users:

| User wants | What they download | What `zeroclaw onboard` does |
|---|---|---|
| CLI only | `zeroclaw` runtime binary | Configure provider, done |
| CLI + Discord | `zeroclaw` runtime binary | Download + install `channel-discord.wasm` |
| Local web UI | `zeroclaw` + `zeroclaw-gw` | Configure both, open browser |
| Desktop app | `zeroclaw-desktop` installer | Bundles runtime + gateway + UI |
| Everything | `zeroclaw-desktop` or `zeroclaw --profile full` | Downloads all plugins |

The `zeroclaw plugin install` command (backed by `PluginHost`, which already exists) becomes the package manager. The `zeroclaw onboard` wizard integrates it so non-technical users never see `cargo`.

#### 4.4.1 Versioning Policy

As ZeroClaw transitions from a single crate to a multi-crate workspace, two concerns must be kept separate from the start:

- **The product version** — what `zeroclaw --version` reports, what GitHub Releases, changelogs, and package managers (Homebrew, apt, cargo-binstall) track. This is the version operators and users reason about.
- **Component stability** — how mature and reliable a given component is. A single version number cannot carry this signal on its own.

These are orthogonal. Conflating them creates misleading semver noise and erodes trust in the version number. This policy defines both.

---

**Crate versioning: unified with intentional exceptions**

All application crates — the kernel, the gateway, tool plugin crates, channel plugin crates, and the CLI — use Cargo workspace package inheritance:

```toml
# Cargo.toml (workspace root)
[workspace.package]
version = "0.8.0"

# crates/zeroclaw-kernel/Cargo.toml
[package]
version.workspace = true
```

A single version in the root `Cargo.toml` is the authoritative product version. This is the right model because:

- Users, operators, and packagers deal with one version, not twelve
- Release automation via `release-plz` is straightforward: one PR, one bump, one changelog entry
- It reflects ZeroClaw's identity as a **product**, not a library ecosystem
- The WIT interface version — not the Rust crate version — is the actual plugin ABI contract (see §5.2)

Three crate classes are intentionally excluded from workspace inheritance and maintain independent versions on their own cadence:

| Crate | Reason for independence |
|---|---|
| `zeroclaw-api` | Starts at `0.1.0`; its `1.0.0` release is a formal milestone deliverable of v1.0.0, signalling a stable Rust trait surface for plugin SDK authors |
| `aardvark-sys`, `zeroclaw-robot-kit` | Hardware library crates with their own user audiences and maintenance cadences; not application components |
| WIT interface files (`wit/*.wit`) | Versioned via `@since` and `@unstable` annotations per the WASI component model spec; these are the primary plugin ABI contract and are independent of Cargo semver entirely |

---

**What "breaking" means for the product version**

Because application crates share a unified version, the team needs a product-level definition of a breaking change — distinct from a breaking change inside a single crate's internal implementation. A breaking change within a plugin crate that does not cross any of the boundaries below is **not** a product-level breaking change and does not warrant a MAJOR bump.

| Bump | Warranted when |
|---|---|
| **MAJOR** | WIT interface changes incompatibly (existing plugins must recompile); kernel IPC API changes incompatibly (gateway or external clients break); config file schema requires a migration; CLI commands or flags are removed or renamed |
| **MINOR** | New capabilities anywhere in the workspace; new plugins available in the registry; new stable APIs; stability tier promotions; deprecation announcements (not removals) |
| **PATCH** | Bug fixes; security patches; documentation corrections; no new capabilities and no deprecations |

---

**Stability tiers**

The product version answers *"what release is this?"* A stability tier answers *"how much can I rely on this component?"* Every component — kernel, gateway, plugin crate, WIT interface — carries one of three tiers. Tiers are documented in the component's `AGENTS.md` and in its plugin registry manifest.

| Tier | Meaning | Implication |
|---|---|---|
| **Stable** | Covered by the product's breaking-change policy. No breaking changes without a MAJOR version bump and a published migration guide. | Kernel (target: v0.8.0), `zeroclaw-api` WIT interface (target: v0.9.0), kernel IPC API (target: v1.0.0) |
| **Beta** | Functional and tested. Breaking changes are permitted in MINOR releases but are announced in the changelog with upgrade notes. | `zeroclaw-gw` (v0.9.0 → v1.0.0), mature channel and tool plugins |
| **Experimental** | No stability guarantee. May break in PATCH releases. Must be clearly marked as `experimental` in docs and plugin registry manifests. | New tool integrations, new channel implementations, early hardware plugins |

Stability tiers are **promoted, never demoted** through a deliberate team decision. Promotions are recorded in the changelog and, for architectural components, in an ADR. A component must hold its current tier for at least one full release cycle before promotion is considered.

---

**Release automation**

Releases use [`release-plz`](https://release-plz.eplant.org/), which opens a release PR on push to `master`, bumps the workspace version, and generates a changelog from conventional commit titles. `release-plz` natively understands workspace inheritance and handles the crate publication order automatically. Crates with independent versions (`zeroclaw-api`, hardware library crates) are managed separately using the same tool's per-crate configuration.

#### 4.4.2 Release Artifacts

The microkernel transition changes the fundamental nature of the question "which features are compiled in?" Today that question has one answer: whatever feature flags you passed to `cargo build`. After the transition it splits into two separate concerns:

- **What is in the kernel binary** — fixed at compile time, determined per platform, published to GitHub Releases
- **What capabilities are available** — determined at runtime by which plugins are installed via `zeroclaw plugin install`

These are no longer the same question, and the current `[features]` section of `Cargo.toml` must be interpreted through that lens.

---

**Fate of the current compile-time feature flags**

The 20+ feature flags in the current `Cargo.toml` fall into three buckets as the architecture matures:

| Bucket | Flags | Outcome |
|---|---|---|
| **Retire → plugin** | `channel-nostr`, `channel-matrix`, `channel-lark`, `channel-feishu`, `whatsapp-web`, `browser-native` | Removed from the kernel. Each becomes a WASM plugin crate published to the plugin registry. No compile-time decision required. |
| **Always-on** | `plugins-wasm`, `skill-creation` | Compiled into every kernel binary unconditionally. `plugins-wasm` is the kernel's core mechanism; `skill-creation` is a zero-overhead code path. Neither belongs behind a flag. |
| **Stay → platform/infrastructure flag** | `peripheral-rpi`, `hardware`, `sandbox-landlock`, `sandbox-bubblewrap`, `voice-wake`, `probe` | Remain as compile-time flags because they require native library linking or OS-level access that cannot be provided by a WASM plugin. `peripheral-rpi` and `hardware` appear only in platform-specific release targets. |

Two flags require a deliberate team decision before the v0.8.0 release and are surfaced here rather than resolved unilaterally:

- **`observability-prometheus`** — currently in `default`. Prometheus metrics add measurable binary size overhead. The question is whether a production runtime should ship observability on by default, or whether operators opt in. Recommendation: keep in `default` for the standard release; operators on severely size-constrained targets can build with `--no-default-features`.
- **`observability-otel`** — OTLP export carries a larger dependency footprint (opentelemetry + reqwest blocking client). Recommendation: remains opt-in, not in `default`. Production deployments that need trace export enable it explicitly.

The `ci-all` meta-feature simplifies substantially as channel and tool flags retire. By v1.0.0 it covers only the remaining platform and infrastructure flags.

---

**The canonical release kernel binary**

The binary published to GitHub Releases for each platform target is built with the following profile:

| Compiled in | Not compiled in |
|---|---|
| Core agent loop | Any channel implementation |
| 10–12 core tools (see Phase 2 D2) | Any non-core tool |
| SQLite + Markdown memory backends | Browser automation |
| Plugin host (`plugins-wasm`, always-on) | `observability-otel` (operator opt-in) |
| `observability-prometheus` | `voice-wake` (libasound2 dependency) |
| `skill-creation` (zero-overhead) | `probe` (niche hardware debugging) |
| IPC server | Web assets (moved to `zeroclaw-gw`) |
| Platform sandbox where supported | `peripheral-rpi` (separate hardware build) |

There is no longer a "build with everything" binary. That mental model is replaced by `zeroclaw plugin install --profile full`, which downloads the full plugin catalog after installing the lean kernel binary.

---

**Release artifact matrix**

Each GitHub Release publishes the following artifacts:

| Artifact | Targets | Notes |
|---|---|---|
| `zeroclaw` kernel binary | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `armv7-unknown-linux-gnueabihf`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` | Static musl build for Linux x86_64; GNU for ARM targets |
| `zeroclaw` kernel binary (hardware) | `aarch64-unknown-linux-gnu`, `armv7-unknown-linux-gnueabihf` | Same targets, compiled with `peripheral-rpi` and `hardware` flags for Raspberry Pi deployments |
| `zeroclaw-gw` gateway binary | Same platform matrix as kernel | Published alongside the kernel; users install separately |
| WASM plugin files | `wasm32-wasip1` | Published to the plugin registry (not GitHub Releases); installable via `zeroclaw plugin install` |
| `zeroclaw-desktop` installer | `x86_64` and `aarch64` for macOS, Windows, Linux (AppImage/deb) | Bundles kernel + gateway + full plugin set; built by the Tauri workflow |

The `wasm32-wasip1` plugin builds run in a separate CI job and are published to the plugin registry on their own cadence. A plugin release does not require a kernel release.

---

### 4.5 The Gateway Separation

The current gateway conflates two things that must be separated:

```
Current (wrong):
  zeroclaw binary
    └── gateway
          ├── Web UI server (serves React app)
          ├── REST/WS/SSE API
          ├── WhatsApp webhook handler  ← this is a channel, not a web server
          ├── WATI webhook handler      ← this is a channel, not a web server
          ├── Linq webhook handler      ← this is a channel, not a web server
          ├── Nextcloud webhook handler ← this is a channel, not a web server
          └── Gmail push handler        ← this is a channel, not a web server

Target (correct):
  zeroclaw-kernel
    └── Local IPC API (Unix socket / 127.x HTTP)

  zeroclaw-gw (separate binary, optional)
    └── Connects to kernel IPC API
    └── Web UI server
    └── REST/WS/SSE API
    └── Generic webhook proxy → routes to channel plugins

  channel-whatsapp.wasm
    └── Registers its own webhook route with the gateway
    └── Handles WhatsApp-specific message parsing
```

**Why this matters:** When the gateway is a separate process, it can crash, restart, or be absent without affecting the agent. The kernel keeps running. This is especially important for the edge hardware use case — a Raspberry Pi running the kernel can have its web UI served from a VPS, with the kernel connecting outbound via a channel plugin. No inbound firewall rules needed.

---

## 5. Standards We Should Adopt

Standards are agreements that have been made by many smart people over many years. Adopting them means we get those years of thinking for free, and it means our software integrates naturally with the rest of the ecosystem. Here are the ones that apply directly to ZeroClaw.

### 5.1 Observability: OpenTelemetry

**What it is:** OpenTelemetry (OTel) is the industry standard for collecting traces, metrics, and logs from software systems. It is maintained by the Cloud Native Computing Foundation and supported by every major cloud provider and monitoring tool.

**Why it matters for ZeroClaw:** We have already implemented `OtelObserver` against our `Observer` trait. We have Prometheus metrics and DORA metrics. The issue is that these are not yet standardized across the codebase — some modules log with `tracing::info!`, others emit `ObserverEvent`s, and the two are not connected.

**What we should do:**
- Adopt OpenTelemetry as the single observability interface for all components
- Ensure every plugin emits OTel spans when it executes, so a user can see a full trace from "message received on Discord" through "agent called shell tool" to "response sent"
- Adopt W3C Trace Context (`traceparent`/`tracestate` headers) for propagating trace IDs across the kernel ↔ gateway ↔ plugin boundary
- Structured log output should be JSON when `ZEROCLAW_LOG_FORMAT=json` is set (already using the `tracing` crate, just needs a JSON subscriber)

**Standards:** OpenTelemetry specification · W3C Trace Context (REC) · RFC 5424 (Syslog, for system log integration)

### 5.2 Plugin Interface: WASI and WIT

**What it is:** WASI (WebAssembly System Interface) is the standard API that WebAssembly modules use to interact with the host system. WIT (WebAssembly Interface Types) is the interface definition language for describing what a WASM component exports and imports — think of it as a `.proto` file but for WASM plugins.

**Why it matters for ZeroClaw:** Our `WasmTool` and `WasmChannel` bridges currently have no formal contract for what a plugin WASM binary must export. This means a plugin author has to guess. WIT files define that contract precisely and enable automatic code generation for plugin authors in any language.

**What we should do:**
- Define WIT interface files for `Tool`, `Channel`, and `Memory` plugin types (a `wit/` directory at the root of the workspace)
- Use `wit-bindgen` to generate the Rust host-side bindings from those WIT files
- Document the WIT interfaces as the official plugin SDK
- A plugin author writes Rust (or Go, or C, or Python) against the WIT interface and `cargo build --target wasm32-wasi` — the result drops into `~/.zeroclaw/plugins/`

**Standards:** WASI 0.2 · W3C WebAssembly Component Model · WIT IDL

### 5.3 Local API: OpenAPI 3.1

**What it is:** OpenAPI is the standard for describing HTTP APIs. Version 3.1 aligns with JSON Schema Draft 2020-12.

**Why it matters for ZeroClaw:** The kernel's local IPC API (the socket that the gateway and other components connect to) needs a stable, documented contract. Without a formal spec, the gateway and kernel will drift apart silently over time.

**What we should do:**
- Write an OpenAPI 3.1 spec for the kernel's local IPC API before implementing it
- Generate the Rust server stubs from the spec using `utoipa` or `aide`
- Publish the spec as `docs/reference/api/kernel-ipc-api.yaml`
- The gateway's external API should also have an OpenAPI spec

**Standards:** OpenAPI 3.1 · JSON Schema Draft 2020-12

### 5.4 Security: OWASP ASVS

**What it is:** The OWASP Application Security Verification Standard is a checklist of security requirements organized by risk level (L1 basic, L2 standard, L3 advanced).

**Why it matters for ZeroClaw:** The gateway handles webhooks from external services, processes untrusted user input, and manages secrets. The pairing system, WebAuthn support, and rate limiting all exist — but there is no framework for verifying that they are complete or correct.

**What we should do:**
- Target ASVS Level 2 for the gateway and security module
- Work through the Level 2 checklist and document which requirements we meet, which we partially meet, and which are out of scope
- Use this as the basis for security-related issues and PRs

**Standards:** OWASP ASVS 4.0 · OWASP Top 10

### 5.5 Quality Model: ISO/IEC 25010

**What it is:** ISO/IEC 25010 defines a model for software product quality with eight top-level characteristics: functional suitability, performance efficiency, compatibility, usability, reliability, security, maintainability, and portability.

**Why it matters for ZeroClaw:** When someone asks "is this good enough to merge?" the answer is currently subjective. ISO 25010 gives us a vocabulary for that conversation. The vision commitments map directly: "zero overhead" → performance efficiency; "any hardware" → portability; "zero compromise" → security + reliability.

**What we should do:**
- Use the eight quality characteristics as a lens in PR reviews for significant changes
- Include a brief quality impact statement in the PR template for architectural changes (e.g., "This change improves maintainability by reducing coupling between the gateway and channel implementations, at no impact to performance efficiency")

**Standards:** ISO/IEC 25010:2023

### 5.6 Already Adopted — Keep These

These are already in place and should be maintained:

| Standard | Status | Where |
|---|---|---|
| Semantic Versioning 2.0.0 | ✅ Adopted | `Cargo.toml`, releases |
| Conventional Commits | ✅ Adopted | `AGENTS.md`, commit history |
| RFC 3339 / ISO 8601 timestamps | ✅ Adopted | `MemoryEntry`, all timestamps |
| XDG Base Directory Specification | ✅ Adopted | `directories` crate in use |
| Keep a Changelog | ✅ Adopted | `CHANGELOG.md` |
| Rust API Guidelines | ✅ Partially | Clippy config enforces many |

---

## 6. Phased Roadmap: v0.7.0 → v1.0.0

Each phase follows the Vision → Architecture → Design → Implementation → Testing → Documentation → Release hierarchy. No phase begins implementation until its design is reviewed and agreed upon.

The overall migration strategy is the **Strangler Fig Pattern**: we grow the new architecture around the edges of the existing code, migrating inward steadily, until the old structure is fully replaced. We never have a "stop the world" rewrite. The application is always shippable.

---

### Phase 1 · v0.7.0 — "The Seams"

**Theme:** Make the architecture visible without changing any behavior. Draw the lines first.

**Why this phase:** You cannot migrate to a layered architecture until the layers exist as real boundaries. Right now, the traits define logical seams but the compiler does not enforce them — everything is in one crate, so anything can import anything. This phase makes the seams real.

**Vision alignment:** None of the vision properties change for users. This is entirely internal. The value is that every future contribution now has a structural home, and new contributors can understand the codebase in parts rather than all at once.

#### Deliverables

**D1: Extract `zeroclaw-api` crate**

Create a new crate `crates/zeroclaw-api` containing only trait definitions and their supporting types. No implementations. No heavy dependencies. This crate should compile in under two seconds.

Move into this crate:
- `src/providers/traits.rs` → `Provider`, `ChatMessage`, `ChatResponse`, `ToolCall`, `StreamChunk`, `ProviderCapabilities`
- `src/channels/traits.rs` → `Channel`, `ChannelMessage`, `SendMessage`
- `src/tools/traits.rs` → `Tool`, `ToolResult`, `ToolSpec`
- `src/memory/traits.rs` → `Memory`, `MemoryEntry`, `MemoryCategory`
- `src/observability/traits.rs` → `Observer`, `ObserverEvent`, `ObserverMetric`
- `src/runtime/traits.rs` → `RuntimeAdapter`
- `src/peripherals/traits.rs` → `Peripheral`

Every other crate in the workspace that needs these types adds `zeroclaw-api` as a dependency. The compiler now enforces that no implementation crate can import another implementation crate without going through the API layer.

**D2: Extract `zeroclaw-tool-call-parser` crate**

The tool call parsing logic in `src/agent/loop_.rs` is approximately 1,400 lines of pure text transformation: it takes a string from the LLM and returns a list of structured tool calls. It has no dependency on agent state, memory, providers, or channels. It handles a dozen different LLM output formats (JSON, XML, GLM-style, MiniMax, Perl-style, markdown fences, and more).

This logic is:
1. Self-contained — perfect for its own crate
2. The most fuzz-testable code in the project — property-based tests belong here
3. A genuine contribution to the Rust ecosystem — no other crate does this comprehensively

Create `crates/zeroclaw-tool-call-parser` with a public API of approximately:

```rust
pub fn parse(text: &str, specs: &[ToolSpec]) -> ParseResult

pub struct ParseResult {
    pub calls: Vec<ParsedToolCall>,
    pub remaining_text: Option<String>,
}

pub struct ParsedToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
    pub tool_call_id: Option<String>,
}
```

The ~300 parsing tests currently in `loop_.rs` move into this crate. `loop_.rs` shrinks by approximately 1,400 lines.

**D3: Adopt OpenTelemetry as the observability standard**

Formalize what is already implemented: document that `ObserverEvent` and `ObserverMetric` are the internal event bus, and that `OtelObserver` is the canonical production backend. Add a JSON structured logging subscriber for `ZEROCLAW_LOG_FORMAT=json`. Adopt W3C Trace Context for future cross-component tracing.

**D4: Write WIT interface files**

Before we implement WASM plugin execution, define the contracts. Create a `wit/` directory at the workspace root with interface definitions for:
- `zeroclaw:tool/tool.wit` — the Tool plugin interface
- `zeroclaw:channel/channel.wit` — the Channel plugin interface

These become the official plugin SDK. The implementation in v0.8.0 will be generated from these files.

#### Success Metrics for v0.7.0

- `zeroclaw-api` compiles in < 2 seconds with zero implementation dependencies
- `zeroclaw-tool-call-parser` has ≥ 95% test coverage (the logic is fully testable in isolation)
- `loop_.rs` is under 8,000 lines
- Zero user-facing behavior changes
- Zero performance regressions (benchmark suite passes)

---

### Phase 2 · v0.8.0 — "The Runtime"

**Theme:** Formalize the agent runtime as a clean, independently deployable unit. Everything that is not the runtime becomes a guest.

**Why this phase:** Once the seams exist (v0.7.0), we can draw the runtime boundary explicitly. This phase extracts `zeroclaw-runtime` as a standalone crate, completes the WASM plugin execution bridge, and wires the plugin registry client — the mechanism by which everything outside the runtime connects to it.

**Vision alignment:** This is where the composition model becomes real for users. A user who wants only a CLI agent downloads one binary, runs `zeroclaw onboard`, and is done — no Rust toolchain, no compilation. The `zeroclaw onboard` wizard gains the ability to download plugin components on demand.

#### Deliverables

**D1: Formalize `zeroclaw-runtime` crate**

Extract the agent orchestration loop, CLI channel, security policy, plugin host, and IPC API into `crates/zeroclaw-runtime`, gated by the `agent-runtime` feature. This crate depends on `zeroclaw-api` and the foundation crates. It has no knowledge of Telegram, Discord, Anthropic, or any specific tool implementation.

The runtime exports a clean public API:

```rust
pub struct Runtime { ... }

pub struct Registry {
    pub fn register_channel(&mut self, ch: Arc<dyn Channel>);
    pub fn register_tool(&mut self, t: Box<dyn Tool>);
    pub fn set_provider(&mut self, p: Arc<dyn Provider>);
    pub fn set_memory(&mut self, m: Arc<dyn Memory>);
    pub fn set_observer(&mut self, o: Arc<dyn Observer>);
}

pub async fn run(runtime: Runtime, registry: Registry) -> anyhow::Result<()>;
```

The binary crate becomes a thin wiring layer that reads config and calls `run`.

**D2: Complete the WASM execution bridge**

Wire Extism into `WasmTool::execute` and `WasmChannel`. The `PluginHost` already handles discovery and installation. The execution bridge is the missing piece. With WIT interfaces defined in v0.7.0, use `wit-bindgen` to generate the host-side bindings.

A complete `WasmTool::execute` implementation using Extism is approximately 30–50 lines. The bulk of the work is defining the host functions that WASM plugins can call (HTTP requests, memory access, logging) within the permission model already defined in `PluginPermission`.

**D3: Component registry client**

Add a `zeroclaw plugin` subcommand backed by a simple registry client:

```
zeroclaw plugin list              # list installed plugins
zeroclaw plugin search <query>    # search the component registry
zeroclaw plugin install <name>    # download, verify, and install a plugin
zeroclaw plugin remove <name>     # remove an installed plugin
zeroclaw plugin update            # update all installed plugins
```

The registry is a JSON index file served from a known URL (e.g., `https://plugins.zeroclawlabs.ai/index.json`). Each entry includes name, version, download URL, SHA-256 checksum, and the publisher's Ed25519 public key. The `PluginHost` signature verification already handles the security model.

**D4: Integrate `zeroclaw onboard` with the plugin system**

The onboarding wizard should ask the user which channels and integrations they want, then call `PluginRegistry::install` for each. No compilation required. The user downloads a binary, runs `zeroclaw onboard`, and has a working configured agent in under two minutes.

**D5: Reduce `all_tools_with_runtime` to core tools only**

The kernel includes exactly the tools a user needs for a useful agent with no plugins installed: `shell`, `file_read`, `file_write`, `file_edit`, `git_operations`, `glob_search`, `content_search`, `memory_recall`, `memory_store`, `memory_forget`, and `web_fetch`. Everything else is registered by installed plugins.

#### Success Metrics for v0.8.0

- `zeroclaw-runtime` compiles independently with no channel or tool implementation code
- `zeroclaw plugin install channel-discord` works end-to-end
- `zeroclaw onboard` installs plugins without requiring a Rust toolchain
- Runtime binary size is **tracked and reported** in the release notes; the aspiration is downward progress toward the vision target (see §7)
- A WASM tool plugin written in Rust using the WIT interface executes correctly

---

### Phase 3 · v0.9.0 — "The Gateway"

**Theme:** Separate the web surface from the agent core.

**Why this phase:** The gateway is currently the largest structural coupling in the codebase. It embeds a compiled React application, handles channel-specific webhook logic, and is compiled into every binary — including binaries intended for $10 edge hardware that will never serve a web page.

**Vision alignment:** This phase delivers the "zero external requirements" promise fully. A user on a Raspberry Pi gets a kernel binary with no web server, no React app, and no HTTP listener. A user who wants the web dashboard installs `zeroclaw-gw` separately.

#### Deliverables

**D1: Define the kernel IPC API**

Before extracting the gateway, define the OpenAPI 3.1 spec for the local API the kernel exposes on a Unix socket or loopback port. This API is what the gateway, the Tauri app, and any future client connects to. It is the stable contract between the kernel and the outside world.

Endpoints include: send a message, receive a streaming response, list active sessions, list installed plugins, get agent status, manage memory, trigger cron jobs. This is a design document first — the spec should be reviewed and agreed upon before a line of implementation is written.

**D2: Implement the kernel IPC server**

Add the IPC server to `zeroclaw-kernel` behind a feature flag (`--features ipc`). On platforms that support it, the kernel listens on a Unix socket at `~/.zeroclaw/kernel.sock`. On Windows, use a named pipe. The `zeroclaw gateway` command (the current entrypoint for the web server) becomes `zeroclaw-gw` connecting to this socket.

**D3: Extract `zeroclaw-gw` as a separate binary**

Move `src/gateway/` to a new `crates/zeroclaw-gw/` crate with its own binary. It depends on `zeroclaw-api` and connects to the kernel via the IPC API. The embedded React application via `rust-embed` moves entirely into this crate — the kernel binary no longer contains any web assets.

**D4: Migrate channel webhook handlers out of the gateway**

The WhatsApp, WATI, Linq, Nextcloud Talk, and Gmail webhook handlers currently in `gateway/mod.rs` move to their respective channel plugins. The gateway provides a generic webhook registration API: a channel plugin, when loaded, registers its webhook path prefix and its handler function. The gateway routes incoming webhooks to the registered handler. The gateway no longer knows about WhatsApp.

**D5: Formalize the Tauri sidecar relationship**

Update `apps/tauri/` to bundle `zeroclaw-gw` as a Tauri sidecar binary. The Tauri app becomes the "full experience" distribution: it starts the kernel and gateway automatically and opens the web UI. Users who download the Tauri app get everything working without touching a terminal.

#### Success Metrics for v0.9.0

- Kernel binary (release) does not contain any web assets or HTTP server code
- `zeroclaw-gw` starts, connects to the kernel via IPC, and serves the web dashboard
- Removing `zeroclaw-gw` does not break the kernel or any channel plugins
- WhatsApp, WATI, Linq, Nextcloud Talk, and Gmail channel code has moved to plugin crates
- Tauri desktop app bundles and starts both binaries correctly

---

### Phase 4 · v1.0.0 — "The Platform"

**Theme:** ZeroClaw becomes a composable platform, not a monolithic application.

**Why this phase:** With the kernel stable, the gateway separate, and the plugin system working, v1.0.0 is the release where the architecture becomes the product. External developers can write and publish plugins. Users can assemble exactly the ZeroClaw they want. The binary can credibly claim the lean profile the vision promises.

#### Deliverables

**D1: Migrate all remaining channels to plugins**

Each of the 27+ channel implementations becomes a standalone WASM plugin crate. They are published to the component registry with signed releases. The kernel binary contains zero channel implementations except the CLI.

**D2: Migrate long-tail tools to plugins**

Approximately 60 of the 70+ tools move to plugin crates, grouped by domain: `zeroclaw-tools-web` (browser, search, screenshot, PDF), `zeroclaw-tools-integrations` (Jira, Notion, Google Workspace, MS365, LinkedIn), `zeroclaw-tools-hardware` (board info, GPIO), `zeroclaw-tools-cloud` (cloud ops, security ops). The kernel retains only the 10–12 core tools identified in v0.8.0.

**D3: Plugin SDK and developer documentation**

Publish a plugin development guide. A developer should be able to write a new tool plugin in an afternoon:
1. Add `zeroclaw-plugin-sdk` as a dependency
2. Implement the WIT-generated trait
3. `cargo build --target wasm32-wasi`
4. `zeroclaw plugin install ./my-plugin/`

The SDK handles the host function bindings, the manifest format, and the permissions model.

**D4: Stabilize the kernel IPC API at v1.0**

The kernel IPC API gets a version prefix (`/v1/`) and a stability guarantee. Breaking changes in v1.x are not permitted to this API. This is the contract that third-party clients and the gateway depend on.

**D5: Extract the versioning policy and stability tier definitions to `docs/contributing/stability-tiers.md`**

The versioning policy and stability tier table defined in §4.4.1 of this RFC become a standing contributor reference document at `docs/contributing/stability-tiers.md`. This document is the day-to-day reference contributors use when assigning a tier to a new plugin crate, and that maintainers consult when making release decisions. The RFC itself remains the historical record of *why* these decisions were made; the extracted document is *what* contributors look up.

#### Success Metrics for v1.0.0

- Runtime binary size is **tracked against the vision target** (see §7); a dedicated optimization pass through each crate is expected as a v1.0.0 workstream
- A third-party developer can publish a working plugin using only public documentation
- All 27+ channel implementations are available as downloadable plugins in the registry
- `zeroclaw onboard` completes a full setup in under 2 minutes on a Raspberry Pi Zero 2W with no Rust toolchain installed
- The full plugin catalog is installable with `zeroclaw plugin install --profile full`

---

## 7. Code and Complexity Metrics

These are estimates based on direct code analysis of the current codebase. They are meant to give a sense of scale, not to be exact predictions.

### Lines of Code Moving Out of the Runtime

| What moves | Approximate lines | Destination |
|---|---|---|
| Tool call parser (from `loop_.rs`) | ~1,400 | `zeroclaw-tool-call-parser` crate |
| 60+ non-core tool implementations | ~30,000 | Plugin crates |
| 24+ non-core channel implementations | ~7,200 | Plugin crates |
| Gateway HTTP server | ~2,260 | `zeroclaw-gw` crate |
| Embedded React app (binary weight) | — | `zeroclaw-gw` crate |
| Channel webhook handlers from gateway | ~500 | Channel plugin crates |
| **Estimated total removed from runtime** | **~41,000 lines** | — |

### File-Level Complexity Reduction

| File | Current lines | Target after migration | Reduction |
|---|---|---|---|
| `src/agent/loop_.rs` | ~9,500 | ~5,000 | ~47% |
| `src/gateway/mod.rs` | ~2,260 | Moves to `zeroclaw-gw` | 100% |
| `src/tools/mod.rs` | `all_tools_with_runtime` is ~680 lines | ~80 lines (core tools only) | ~88% |
| `src/providers/mod.rs` | ~3,750 | ~1,200 (providers self-register) | ~68% |
| `src/channels/mod.rs` | ~200 + 44 channel files | CLI channel only | ~90% |

### Binary Size — Measured Progress and Vision Target

The project's vision is expressed in runtime terms: **<5 MB RAM** on $10 hardware. Binary size on disk and runtime memory footprint (RSS) are related but not identical — demand paging means only executed code paths are resident. Both are tracked.

> **Two-pass model:** Architectural decomposition (Phases 1–3) and binary size optimization are separate workstreams. Decomposition *enables* optimization by isolating dependencies to their owning crates. Maximizing efficiency crate-by-crate is the expected second pass, not a deliverable of the structural work itself.

| Configuration | Pre-decomposition (v0.6.x) | Phase 1 result (v0.7.0) | Vision target |
|---|---|---|---|
| Full monolith binary | ~8.8 MB | N/A (replaced by plugin model) | N/A |
| Foundation only (`--no-default-features`) | N/A | **6.6 MB** *(measured, stripped)* | TBD after optimization pass |
| Runtime binary (foundation + `agent-runtime`) | N/A | tracked → | aspiration: ≤ 5 MB RAM at runtime |
| Runtime + gateway | N/A | tracked → | ~5–7 MB on disk |
| Runtime + gateway + top 5 channels | N/A | tracked → | ~8–10 MB (plugins are separate files) |
| Tauri desktop app (bundles all) | N/A | tracked → | ~20–25 MB installer |

The 6.6 MB Phase 1 foundation build represents real progress from the 8.8 MB monolith and proves the decomposition is working. Reaching the vision target requires a dedicated dependency-audit and optimization pass through each crate after the structural decomposition is complete — reviewing each crate's `Cargo.toml` for unnecessary or over-featured dependencies, validating LTO and strip profiles, and auditing which tokio/serde feature flags are actually needed.

The key structural shift: binary size stops being a function of "features compiled in at build time" and becomes a function of "plugins installed at runtime," which the user controls. That shift is the architectural goal of Phases 1–3. The size numbers are the optimization goal of the pass that follows.

### Compilation Time Improvement

Currently, a full `cargo build --release` on this codebase compiles every channel, every tool, every provider, and the embedded React app in a single compilation unit. Crate decomposition means:

- The kernel compiles independently and its compiled output is cached
- A change to `channel-discord` does not recompile the kernel
- Contributors working on a plugin only recompile their plugin
- CI can parallelize crate compilation across jobs

Estimated wall-clock time improvement for incremental builds: 60–75% reduction for changes that do not touch the kernel.

---

## 8. What This Means for Contributors

### For new contributors

The most common complaint from new contributors to large codebases is: "I don't know where to start." With the current architecture, the answer to "where does a Discord message go?" requires tracing through `channels/discord.rs` → `channels/mod.rs` → `gateway/mod.rs` → `agent/loop_.rs` → dozens of other files.

With the microkernel architecture, the answer is: "it goes to the kernel's `Channel` receiver, via the `channel-discord` plugin." A new contributor can understand the Discord channel completely by reading one plugin crate. They can understand the full agent loop by reading `zeroclaw-kernel` without any channel or tool code in scope.

**A good rule of thumb for new contributors:** if you can describe your change in one sentence without mentioning more than one component, you are working at the right level. "Fix a bug in how the Discord channel handles thread replies" is one component. "Refactor the agent loop and update the Discord channel and also fix the memory backend" is three components — it should be three PRs.

### For maintainers

Every bug report will have a clear home. "The agent is calling tools incorrectly" → `zeroclaw-tool-call-parser` or `zeroclaw-runtime`. "The Discord integration is broken" → `channel-discord` plugin. "The web dashboard is not loading" → `zeroclaw-gw`. Right now, any of those bugs could be anywhere in 50,000+ lines.

### For the release process

The plugin model means channels and tools can have independent release cycles. A bug fix in the Telegram channel does not require a new kernel release. The kernel's stability becomes the foundation that everything else builds on. Rapid iteration on plugins does not risk kernel stability.

### For the community

A published WIT interface and plugin SDK means anyone can extend ZeroClaw without forking it. A company that needs a specific integration can write a plugin against the public interface. This is how ecosystems are built.

---


---

## Appendix A: Glossary

Terms used in this document that may be unfamiliar:

**Big Ball of Mud** — An architecture (or lack thereof) in which the codebase has grown organically without structural planning. The name comes from a 1997 paper by Brian Foote and Joseph Yoder. It is the most common architecture in software, not because anyone chooses it, but because it is what you get by default.

**Conway's Law** — "Any organization that designs a system will produce a design whose structure is a mirror image of the organization's communication structure." (Mel Conway, 1968) If contributors work in isolated silos without talking to each other, the code will reflect that. If contributors collaborate with clear interfaces between their work, the code will reflect that too.

**Dependency Inversion Principle** — High-level modules should not depend on low-level modules. Both should depend on abstractions. This is why `zeroclaw-runtime` depends on `zeroclaw-api` (abstractions) and not on `channel-discord` (a specific implementation).

**Microkernel** — An architecture in which the core system contains only the minimum necessary functionality, and all other capabilities are provided by separate components that communicate with the core through well-defined interfaces.

**Strangler Fig Pattern** — A migration strategy in which you incrementally replace parts of an existing system by building new components alongside the old ones. Named for the strangler fig plant, which grows around an existing tree until the original tree has been fully replaced. The key property: the system is always running and always deployable during the migration.

**Technical Debt** — The accumulated cost of taking shortcuts in software design. Like financial debt, a small amount can be productive (you ship faster now). A large amount becomes crippling (you spend all your time on interest payments — i.e., bug fixes and workarounds — instead of new features).

**WIT (WebAssembly Interface Types)** — An interface definition language for describing what WASM components export and import. Think of it as a contract: "a Tool plugin must export a function called `execute` that takes JSON and returns JSON." WIT makes that contract precise and machine-readable.

---

## Appendix B: Further Reading

These are resources the team may find valuable. They are not required reading, but each one has directly influenced this proposal.

- **"A Philosophy of Software Design"** — John Ousterhout. The best short book on managing complexity in software. His concept of "deep modules" (simple interfaces, powerful implementations) is exactly what the microkernel model aims for.

- **"Clean Architecture"** — Robert C. Martin. The Dependency Rule described in Section 4.2 of this document comes from this book.

- **"Release It!"** — Michael Nygard. Practical patterns for building software that stays up in production. The gateway separation and circuit-breaker patterns discussed here are drawn from this book.

- **The Rust API Guidelines** — https://rust-lang.github.io/api-guidelines/ — The official guide for designing idiomatic Rust libraries. Our trait interfaces should follow these conventions.

- **The WebAssembly Component Model** — https://component-model.bytecodealliance.org/ — The technical foundation for the plugin system proposed in this RFC.

- **OpenTelemetry specification** — https://opentelemetry.io/docs/specs/ — The full specification for the observability standard we are adopting.

---

*This proposal was developed from a detailed analysis of the ZeroClaw codebase at v0.6.8. The code metrics cited are based on direct measurement of the source files. The architectural recommendations reflect established patterns in systems software design applied to the specific constraints and goals of the ZeroClaw project.*

*Feedback, corrections, and counterproposals are welcome. The best architecture is the one the team understands and believes in — not the one any single person dictated.*
