# Requirements: osAgent

**Defined:** 2026-06-12
**Core Value:** Wizard cannot exfiltrate Vault secrets via an MCP server because the wizard binary has compile-time zero MCP, verified by a 4-layer CI gate (source-grep + `nm --defined-only` + `cargo-bloat --crates` + `strings`).

This document scopes **M1 — Foundation** only. M2/M3/M4 requirements are listed under v2 with full enumeration of intent. They become M2/M3/M4 v1 requirements when those milestones begin via `/gsd:new-milestone`.

---

## v1 Requirements (M1 — Foundation)

### Fork & Attribution

- [ ] **FORK-01**: Public GitHub fork `andreas2301/osAgent` of `zeroclaw-labs/zeroclaw` exists, working branch is `osagent-main`, upstream remote configured for `git fetch upstream`. NOTICE preserves zeroclaw attribution + adds osAgent fork attribution; `LICENSE-APACHE` + `LICENSE-MIT` from upstream retained.
- [ ] **FORK-02**: `sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md` exists and documents the quarterly merge procedure: cadence (1st week of Q1/Q2/Q3/Q4), diff-stat budget per merge, conflict-resolution log format, out-of-cycle critical-security-fix criteria, append-only conflict log convention.
- [ ] **FORK-03**: `cargo deny` is configured with: (a) license allowlist limited to MIT/Apache-2.0/BSD-2/BSD-3/ISC/Unicode-DFS-2016 — explicitly banning AGPL-3.0 (presage, libsignal-service, libsignal) and WTFPL (frankenstein); (b) advisory database current; (c) `cargo deny check` runs in CI on every PR.

### Workspace Restructure (binary split)

- [ ] **WS-01**: Cargo workspace builds via `cargo build --workspace` with `resolver = "2"` pinned. Two binaries `osagent-engineer` and `osagent-wizard` build cleanly. Workspace contains a `bins/` directory with two ~50-LOC `main.rs` crates whose `Cargo.toml` is the human-readable manifest of compiled-in capabilities.
- [ ] **WS-02**: **MCP exclusion is structural, not feature-flagged.** A dedicated crate `osagent-tools-mcp` (or similar name) holds all MCP code. `bins/wizard/Cargo.toml` has zero `mcp` references (no dependency, no feature, no optional, no cfg gate). The engineer's `Cargo.toml` explicitly depends on `osagent-tools-mcp`. Verified by reading the manifests in code review.
- [ ] **WS-03**: 4-layer CI gate enforces wizard-no-MCP property on every PR and release build:
  - Layer 1: `grep -r "mcp" bins/wizard/` and the wizard's `Cargo.toml` dependency tree returns no matches
  - Layer 2: `nm --defined-only target/release/osagent-wizard | grep -i mcp` is empty
  - Layer 3: `cargo bloat --crates --release --bin osagent-wizard` does not list any `mcp` crate
  - Layer 4: `strings target/release/osagent-wizard | grep -iE "(mcp_|model.context.protocol)"` is empty
  - All four layers must pass; any single failure breaks the build.
- [ ] **WS-04**: Workspace uses **explicit registration** for channels/providers/tools (single `pub fn register(reg: &mut Registry)` entry point per crate, called from `bins/<binary>/main.rs`). NO use of `inventory!` / `linkme!` distributed-slice patterns (these defeat the structural exclusion via auto-discovery at link time).
- [ ] **WS-05**: All workspace dependencies use `default-features = false` and explicit feature lists. Workspace inherits via `dep:` prefix-style declarations. Feature unification audit script (`cargo tree --duplicates`) runs in CI.

### Strip Dead Surface (whole crates)

- [ ] **STRIP-01**: Workspace member list drops these upstream crates entirely: `zeroclaw-hardware`, `robot-kit`, `aardvark-sys`, `apps/tauri`, `zeroclaw-plugins`. Source directories removed. `cargo build --workspace` still succeeds.
- [ ] **STRIP-02**: Channel implementations stripped to 6 (keep: Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal). 26 others removed at source level (Discord, IRC, Email/IMAP, Gmail-Push, voice-call, voice-wake, WhatsApp-Web/Selenium, Twitter, Reddit, Bluesky, Nostr, LINE, WeChat, WeCom, QQ, DingTalk, Lark, Feishu, ClawdTalk, Nextcloud, Linq, WATI, iMessage, MoChat, Notion, MQTT, ACP-server, **webhook**). Trait registry entries deleted.
- [ ] **STRIP-03**: Provider implementations stripped to 5 (keep: Anthropic, Gemini, Kimi-code via OpenAI-compatible base, Ollama, OpenRouter). ~50 other providers removed at source level. `provider_aliases.rs` deleted.
- [ ] **STRIP-04**: Tool implementations stripped (~35 tools removed): all browser tools, web_search, web_fetch, screenshot, weather, jira, notion, google_workspace, microsoft365, linkedin, composio, image_gen, canvas, hardware_*, claude_code_runner, swarm, claude_code, gemini_cli, codex_cli, opencode_cli, project_intel, discord_search, http_request, pushover, reaction, llm_task, escalate (in-band), skillforge, skill_improve, skill_http, voice_*.
- [ ] **STRIP-05**: Gateway sub-surface stripped: REST endpoints (config, onboarding, pairing, personality, plugins, webauthn), ACP bridge, SSE, embedded web dashboard, pairing dashboard UI, mTLS server option, outbound webhook endpoints. **`/ws/chat` endpoint and `paired_tokens` auth path KEPT** (OS-MDashboard's `chat-relay.ts` depends on them). Gateway crate retained but trimmed to the minimal axum router.
- [ ] **STRIP-06**: Webhook channel explicitly NOT included in v1 (user rejection on security grounds). Source code removed; `cargo deny` includes a check to prevent re-introduction without a documented decision.
- [ ] **STRIP-07**: i18n Mozilla Fluent pipeline retained, non-en locales stripped (~12 translation files in `translations/` deleted). en-US `.ftl` files retained as the authoritative source.

### Telemetry & Manifest

- [ ] **TELEMETRY-01**: Audit zeroclaw codebase for phone-home patterns: search for `reqwest::Client::new` + `sentry::` + `posthog::` + `honeycomb::` + env-driven URLs (`SENTRY_DSN`, `POSTHOG_KEY`, etc.) + any outbound HTTPS to non-customer-controlled domains. Findings documented in `docs/telemetry-audit.md`. All identified phone-home paths removed. `cargo deny` updated to ban sentry / posthog / honeycomb / opentelemetry-exporter-otlp-http reqwest-based crates as direct deps.
- [ ] **MANIFEST-01**: Build emits `MANIFEST.toml` listing every compiled-in channel, provider, and tool, with two sections:
  - `[declared]`: derived from `CARGO_FEATURE_*` env vars and Cargo.toml dependency tree at build time
  - `[detected]`: derived from post-link symbol analysis (`cargo bloat --crates`)
  - A build-time CI check enforces `[declared] == [detected]`; any divergence is a CI failure (catches "feature was declared but code was orphaned" and "code was linked but not declared")
- [ ] **MANIFEST-02**: `osagent manifest --diff <config.toml>` CLI subcommand validates that every channel/provider/tool referenced in the config exists in the binary's manifest. Refuse-to-start on mismatch (e.g., wizard config that references `[mcp]` when wizard binary's MANIFEST lacks MCP). Available on both binaries.
- [ ] **MANIFEST-03**: Reproducible-build profile pinned: `[profile.release]` sets `codegen-units = 1`, `lto = "fat"`, `strip = "symbols"`, `panic = "abort"`. CI builds with `CARGO_INCREMENTAL=0`. Two-run byte-equality assertion in the release job (mitigates rust-lang/rust#150462 non-deterministic DCE).

### Install-Guide Drop-In

- [ ] **INSTALL-01**: `sovereign-shield-install-guide/ansible/install_osagent.yml` created as a structural template for the M2-completing engineer-binary install. Respects all 14 install-guide invariants (phase ordering, mTLS cert provisioning pattern, sandbox-allowed home-mirror cert paths, ExecStartPre preflight, AMQP env file pattern, pre-create audit log file before non-root daemon opens it). PR opened against `sovereign-shield-install-guide` main but NOT merged at M1 (engineer binary not yet at parity; merge happens at M2 close). Plan-then-execute discipline followed: a Touch/Change/Impact/Rollback plan posted in the PR description before any ansible file is touched.

---

## v2 Requirements (M2 — Engineer binary production-ready)

Deferred to M2. Tracked for visibility.

### Engineer runtime additions
- **ENG-BRIDGE**: Native AMQP `bridge` tool replaces shell-invoked engineer-amqp-bridge. Uses `lapin` with manual `tokio-rustls` stream for ServerName override (`rabbitmq.shield.internal` SAN, dial `127.0.0.1`). Operator allowlist (`/etc/zeroclaw/operator/allowlist.json`) validated at startup, fail-closed if missing.
- **ENG-EXCHANGE**: First-class `exchange` channel implementation. Native PLAN/MISSION/REPORT envelope schemas. Replaces ad-hoc file-polling in HEARTBEAT.md.
- **ENG-LIFECYCLE**: Pause-marker and activation-marker as daemon primitives. CancellationToken passed into every tool. Vault writes complete current transaction on pause then halt.
- **ENG-SQLCIPHER**: Memory backend uses `rusqlite` + `bundled-sqlcipher-vendored-openssl`. Key derived from `customer_id + vault-supplied-salt`. Cross-customer memory restore fails fast.
- **ENG-AUDIT**: Hash-chained dual-sink audit log: journald + append-only file per customer at `/var/log/sovereign-shield/osagent-<customer_id>.audit`. Daily anchor cross-linked to witness's chain.
- **ENG-CHANNELS-RT**: Mattermost (in-house `reqwest` wrapper around v4 REST + WS) and Matrix (`matrix-sdk` 0.18) channel runtime implementations.
- **ENG-CHANNEL-ROLES**: `ops` (can trigger actions) vs `observer` (read-only) role allowlist per channel.
- **ENG-PARITY**: Functional parity audit with current zeroclaw engineer; engineer cutover in install-guide; production deployment.

### Engineer migration
- **MIG-ENG**: install-guide ansible task swapped from `install_zeroclaw.yml` to `install_osagent.yml` (engineer-only). Existing engineer installs migrate on next ansible apply. Validation: smoke test passes on clean VM AND on upgrade-in-place.

## v3 Requirements (M3 — Wizard binary + subagent system)

Deferred to M3.

### Wizard binary
- **WIZ-BIN**: Wizard binary builds with `osagent-tools-mcp` NOT in its dependency tree. CI 4-layer gate passes on every build.
- **WIZ-VAULT**: Vault tool with idempotency keys (hash of tool + args + correlation_id), customer-prefix path enforcement (`secret/data/<customer_id>/...`), structured approval-required wrapper. Uses `vaultrs` 0.8 + KV v2 + AppRole.
- **WIZ-2P**: 2-person approval primitive in Rust runtime: dashboard ack + chat ack from distinct identities; 1h timeout escalates to sysadmin chat (no auto-approve, no silent expiry).
- **WIZ-BOOT**: Bootstrap secret: sealed plaintext on disk mode 0600 root:wizard, loaded only when Vault unreachable at startup. Documented in `documentation/osAgent/BOOTSTRAP.md`.
- **WIZ-CHANNELS**: Dashboard WS (already works) + Telegram + Slack + customer-chosen one of {Mattermost, Matrix, WhatsApp-Cloud, Signal} active for wizard binary.

### Subagent primitive
- **SUB-FORMAT**: Markdown frontmatter format (Claude-Code convention). Parsed via `gray_matter` 0.3.
- **SUB-POOL**: Pool cost semantics — parent's daily cap is the shared pool. NOT per-subagent.
- **SUB-DEPTH**: One level deep enforced at primitive level (no grand-subagents).
- **SUB-AUDIT**: Both parent + subagent identity in audit log (`engineer/secret-rotation-planner`).
- **SUB-SIGN**: Subagent prompts signed by wizard's git commit signature (ssh-key 0.6 `SshSig` parses `git -c gpg.format=ssh` format); engineer verifies before invoke.
- **SUB-ISOLATE**: Subagent runs in separate Tokio task with own `CancellationToken`. No grand-children. Cost pool drains from parent's daily cap.

## v4 Requirements (M4 — Channels + Ops + Provider routing + Production rollout)

Deferred to M4.

### Remaining channels
- **CHAN-WA**: WhatsApp-Cloud in-house wrapper (no maintained Rust crate; in-house 200-LOC `reqwest` wrapper around Meta Graph API).
- **CHAN-SIGNAL**: Signal channel runs `signal-cli` (GPLv3 Java daemon) as a separate process. JSON-RPC over Unix socket. License boundary is the process edge (mere-aggregation, not derived work). Java/JVM ansible provisioning ships with install task. AGPL Rust SDKs explicitly banned in `cargo deny`.
- **CHAN-OUTBOX**: SQLite per-channel outbox; replay on reconnect. Survives "Telegram country-block + dashboard down" combo.
- **CHAN-ROTATE**: `osagent rotate-channel-secret --channel=<name>` CLI rotates bot tokens in Vault + restarts process + updates external app config.

### Codeword challenge
- **CHAL-01**: High-risk tool calls emit 4-word phrase shown in dashboard, require confirm-reply in channel. Configurable risk threshold per tool.

### Provider routing
- **PROV-MODES**: Three provider policy modes — `cloud-first` (default), `local-first` (oracle primary, cloud fallback), `local-only` (hard wall — refuse to serve if oracle unreachable, emit alert, NEVER silent failover). Type-level separation enforced (LocalOnlyProvider vs FallbackProvider) so `local-only` cannot accidentally call cloud.
- **PROV-ORACLE**: `[providers.models.oracle]` config entry for ola-management-oracle (Ollama-compatible local LLM proxy).

### Operational CLIs
- **OPS-RESCUE**: `osagent-rescue` CLI: same operator allowlist, no LLM, direct AMQP. Saves you when daemon is crash-looping at 3am.
- **OPS-ROTATE**: `osagent rotate-channel-secret` CLI (see CHAN-ROTATE).
- **OPS-MANIFEST**: `osagent manifest --diff` CLI (see MANIFEST-02; first shipped at M1 but extended in M4 for the wizard-binary case).

### Documentation & production rollout
- **DOC-FULL**: `sovereign-shield-backup/documentation/osAgent/` complete: architecture, bootstrap, approval flow, audit format, subagent spec, rotation runbook, rescue runbook, upstream sync runbook, channel onboarding per customer. Arc42 numbering convention.
- **MIG-WIZ**: install-guide ansible task adds `install_osagent.yml` wizard binary deployment alongside engineer. Both running in production. Old zeroclaw binaries removed from PATH and `/usr/local/bin/zeroclaw` symlink purged.

---

## Out of Scope

| Feature | Reason |
|---------|--------|
| **Microsoft Teams channel** | Not in zeroclaw v0.7.5; requires Bot Framework + Azure AD app (2–3 weeks net-new). Defer to post-v4. |
| **APAC corporate channels** (Lark/Feishu, WeCom, DingTalk, WeChat, QQ, LINE) | No customer demand; revisit when APAC GTM begins. |
| **Webhook ingress channel** | User rejection on security grounds: "rather build a custom osAgent update for a weird stack than overengineer and give the capabilities to get hacked." Custom integration per weird-stack customer instead. |
| **WhatsApp Web (Selenium scraper)** | Brittle, TOS-violating. WhatsApp-Cloud only. |
| **In-process Rust Signal SDK** | All available Rust Signal crates (presage, libsignal-service, libsignal) are AGPL-3.0; including any forces entire osAgent under AGPL. signal-cli subprocess is the only license-compatible path. |
| **Custom Landlock sandbox re-enable** | v1 keeps `sandbox.enabled=false` matching current install-guide. Auto-detect chain explicitly disabled. v2 candidate. |
| **Subagent grand-children (depth > 1)** | Fork-bomb hazard; one-level enforced. |
| **Outbound webhook subscriptions** | Same security posture as inbound webhook channel. |
| **Public artifact distribution** (signed binaries on GitHub Releases) | Ship via our own infrastructure first; v2. |
| **Multi-tenancy inside one osAgent** | Design assumes one customer = one platform = one engineer + one wizard. Multi-tenancy at config layer rejected at config-load. |
| **MCP on wizard binary** | Compile-time-prohibited (structural crate exclusion + 4-layer CI gate). Load-bearing safety property. |
| **Coexist forever** (old zeroclaw + osAgent both supported) | Explicit sharp cutover; coexist doubles maintenance forever. |
| **Browser tool, web_search, web_fetch, image gen, voice (Call+Wake), PDF RAG, WebAuthn, Postgres/Qdrant memory, embeddings consolidation, community skill HTTP fetch** | None used by current install-guide; zero customer demand. |
| **Cargo `inventory!` / `linkme!` distributed-slice registration** | Auto-discovery at link time defeats the structural MCP exclusion. Explicit registration only. |
| **AGPL-licensed dependencies (transitively or directly)** | License contamination would force entire osAgent under AGPL. `cargo deny` enforces license allowlist. |

---

## Traceability

Empty initially. Filled by roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| FORK-01 | Phase 1.1 | Pending |
| FORK-02 | Phase 1.1 | Pending |
| FORK-03 | Phase 1.1 | Pending |
| WS-01 | Phase 1.2 | Pending |
| WS-02 | Phase 1.3 | Pending |
| WS-03 | Phase 1.3 | Pending |
| WS-04 | Phase 1.2 | Pending |
| WS-05 | Phase 1.2 | Pending |
| STRIP-01 | Phase 1.4 | Pending |
| STRIP-02 | Phase 1.5 | Pending |
| STRIP-03 | Phase 1.5 | Pending |
| STRIP-04 | Phase 1.5 | Pending |
| STRIP-05 | Phase 1.6 | Pending |
| STRIP-06 | Phase 1.4 | Pending |
| STRIP-07 | Phase 1.5 | Pending |
| TELEMETRY-01 | Phase 1.4 | Pending |
| MANIFEST-01 | Phase 1.5 | Pending |
| MANIFEST-02 | Phase 1.5 | Pending |
| MANIFEST-03 | Phase 1.5 | Pending |
| INSTALL-01 | Phase 1.6 | Pending |

**Coverage:**
- v1 (M1) requirements: 20 total
- Mapped to phases: 20
- Unmapped: 0 ✓

---
*Requirements defined: 2026-06-12*
*Last updated: 2026-06-12 after initialization (incorporates 4-researcher findings + 3 cross-cutting refinements)*
