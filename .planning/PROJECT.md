# osAgent

## What This Is

A tailored fork of zeroclaw v0.7.5 producing two compile-time-separated binaries — `/usr/local/bin/engineer` and `/usr/local/bin/wizard` — that drop into the sovereign-shield platform's existing systemd units to replace the current generic zeroclaw deployment. The engineer is the platform-maintenance agent (talks via Telegram/Slack to the customer's ops team, executes privileged actions through the operator AMQP bridge); the wizard is the planner/install-guide agent that writes secrets to Vault, reachable via OS-MDashboard plus a customer-chosen chat (Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, or Signal).

## Core Value

**Wizard cannot exfiltrate Vault secrets via an MCP server because the wizard binary has compile-time zero MCP, verified by CI (`nm osagent-wizard | grep -i mcp` must return empty).** Every other property — channels, providers, sandbox shape, subagents, audit — flows from "agents that touch high-value secrets must have a provably small attack surface."

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

(None yet — ship to validate)

### Active

<!-- Current scope. Building toward these in M1 (Foundation). M2/M3/M4 requirements added when those milestones start. -->

**M1 — Foundation (this milestone):**

- [ ] **FORK-01**: Public GitHub fork `andreas2301/osAgent` exists, attribution preserved (`LICENSE-APACHE`, `LICENSE-MIT`, `NOTICE` from upstream + osAgent `NOTICE`), branch `osagent-main` is the working branch
- [ ] **FORK-02**: Quarterly upstream-sync runbook documented in `sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md`
- [ ] **WS-01**: Workspace builds with two binaries (`osagent-engineer`, `osagent-wizard`) via Cargo features (`engineer-bin`, `wizard-bin`); both binaries link the same shared crates; `cargo build --bin osagent-engineer --features engineer-bin` and the wizard variant both succeed
- [ ] **WS-02**: CI gate enforces `nm osagent-wizard | grep -i mcp` returns empty (build fails otherwise)
- [ ] **STRIP-01**: Whole crates dropped: `zeroclaw-hardware`, `robot-kit`, `aardvark-sys`, `apps/tauri`, `zeroclaw-plugins`. Workspace still compiles.
- [ ] **STRIP-02**: 26-of-30 channels stripped (keep: Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal — others gated out at feature-flag and source level)
- [ ] **STRIP-03**: ~50-of-60 providers stripped (keep: Anthropic, Gemini, Kimi-code via openai-compatible base, Ollama, OpenRouter)
- [ ] **STRIP-04**: ~35-of-60 tools stripped (browser, web_search, web_fetch, hardware_*, weather, jira, notion, google_workspace, microsoft365, linkedin, composio, image_gen, canvas, claude_code_runner, swarm, gemini_cli, codex_cli, opencode_cli, project_intel, discord_search, http_request, pushover, reaction, llm_task, escalate, skillforge, skill_improve, skill_http, voice_*, screenshot)
- [ ] **STRIP-05**: Gateway sub-surface dropped (REST endpoints config/onboarding/pairing/personality/plugins/webauthn, ACP bridge, SSE, embedded web dashboard, pairing dashboard UI, mTLS server option, outbound webhook endpoints) — **but `/ws/chat` endpoint and paired_tokens auth path KEPT** because OS-MDashboard's chat-relay depends on them
- [ ] **STRIP-06**: Webhook channel explicitly NOT included (user rejected on security grounds — "rather build a custom osAgent update for a weird stack than overengineer and give the capabilities to get hacked")
- [ ] **TELEMETRY-01**: Audit zeroclaw codebase for phone-home telemetry; strip all outbound metrics/error-reporting to third-party services; document findings
- [ ] **MANIFEST-01**: Build emits `MANIFEST.toml` listing every compiled-in channel, provider, and tool; ships with each binary
- [ ] **INSTALL-01**: `sovereign-shield-install-guide/ansible/install_osagent.yml` created as a drop-in replacement for `install_zeroclaw.yml`, structured for engineer-only at M1 (wizard still uses old zeroclaw until M3 ships)

**M2/M3/M4 — added when those milestones begin** (see ROADMAP.md for milestone scope).

### Out of Scope

<!-- Explicit boundaries with reasoning. -->

- **Microsoft Teams channel** — not in zeroclaw v0.7.5; requires Bot Framework + Azure AD app (2–3 weeks net-new work). Defer to v2.
- **APAC corporate channels (Lark/Feishu, WeCom, DingTalk, WeChat, QQ, LINE)** — no customer demand yet; revisit when APAC GTM begins.
- **Webhook channel** — security risk; user prefers per-stack custom integration over generic ingress.
- **WhatsApp Web (Selenium scraper)** — brittle, TOS-violating; WhatsApp-Cloud only.
- **Custom Landlock sandbox re-enable** — v1 keeps `sandbox.enabled=false` matching current install-guide. v2 candidate.
- **Subagent grand-children (depth > 1)** — fork-bomb hazard; one-level deep enforced.
- **Outbound webhook subscriptions** — same security posture as inbound Webhook channel.
- **Public artifact distribution (signed binaries on GitHub Releases)** — ship via our own infrastructure first. v2.
- **Multi-tenancy inside one osAgent instance** — design assumes one customer = one platform = one engineer + one wizard. Multi-tenancy rejected at config-load time.
- **MCP on wizard binary** — compile-time-prohibited. Vault-write safety property.
- **Coexist forever (old zeroclaw + osAgent both supported)** — explicit sharp cutover; existing installs migrate on next ansible apply. Coexist doubles maintenance forever.
- **Browser tool stack, web_search, web_fetch, image generation, voice (Voice Call + Voice Wake), PDF RAG, WebAuthn, Postgres/Qdrant memory backends, embeddings consolidation, community skill HTTP fetch** — none used by current install-guide; zero customer demand.

## Context

**Project ecosystem:** sovereign-shield is a self-hosted single-tenant security platform (one customer = one deployment). The platform already runs zeroclaw v0.7.5 as both the engineer and wizard agents (pinned earlier in this session via `feat/pin-zeroclaw-v0.7.5` → `main` of `sovereign-shield-install-guide`, SHA256-verified, `git -C` regression fixed in v0.7.5).

**Why fork instead of using upstream:** zeroclaw is a general-purpose mass-market AI agent (60+ providers, 30+ channels including consumer IoT/voice/social, hardware crate, desktop app, WASM plugins). 90%+ of the binary is dead weight in our deployment. More importantly, the upstream binary has surfaces — REST endpoints, plugin loader, sandbox auto-detect chain that wraps shell in Docker, MCP server registry — that we'd have to disable via config and hope nobody flips a flag. Forking lets us drop those at the crate/feature level so the attack surface is provably small.

**Why two binaries:** engineer and wizard have different threat models. Engineer does platform-maintenance ops through an AMQP bridge to the operator service (45-verb allowlist); wizard writes to Vault. Sharing one binary with config-based feature toggles means a config drift can silently grow the wizard's surface. Compile-time separation enforces the property: wizard cannot run an MCP server because the code isn't compiled in.

**Existing infrastructure to integrate with:**
- `sovereign-shield-install-guide` (ansible deployment, 14 documented anti-pattern classes, plan-then-execute hard rule)
- `OS-MDashboard` (Next.js dashboard with `chat-relay.ts` → zeroclaw's `/ws/chat` bearer-auth endpoint — keeps working unchanged)
- `ola-host-engineer-config` (engineer's life-config: skills, RAG, scheduled jobs, HEARTBEAT.md audit loop)
- `ola-management-wizard-config` (wizard's life-config: 5 RAG docs, 1 scheduled job, customer-veil identity)
- `ola-management-operator` (operator-service + engineer-amqp-bridge — 45-verb allowlist at `/etc/zeroclaw/operator/allowlist.json`)
- `ola-management-oracle` (Ollama-compatible local LLM proxy — becomes the `oracle` provider entry for local-first/local-only customers)
- `ola-management-witness` (audit aggregation point — receives osAgent's hash-chained audit lines)
- `sovereign-shield-backup` (Arc42-style documentation home; osAgent docs land under `documentation/osAgent/`)

**42 architectural decisions ratified before this initialization** (transcribed below as constraints — these are NOT to be re-litigated in discuss-phase):

| # | Decision |
|---|---|
| 1 | Two binaries via Cargo features (`engineer-bin`, `wizard-bin`); shared workspace crates; compile-time MCP exclusion on wizard |
| 2 | Teams channel deferred to v2; v1 channel set: Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal |
| 3 | OS-MDashboard channel = existing chat-relay → gateway WS (no new channel impl needed) |
| 4 | Path B execution = true GitHub fork (`andreas2301/osAgent`), our changes on `osagent-main`, quarterly upstream merges |
| 5 | Vault write approval: 2-person ack hard-coded in Rust runtime (dashboard + chat, distinct identities) |
| 6 | Vault path enforcement: every write asserts path starts with `secret/data/<customer_id>/` |
| 7 | Bootstrap secret: sealed plaintext on disk mode 0600 root:wizard, loaded only when Vault unreachable |
| 8 | Vault writes use idempotency keys (hash of tool + args + correlation_id) |
| 9 | Memory: sqlcipher with customer-derived key (cross-customer restore fails fast) |
| 10 | High-risk tool calls require 4-word codeword challenge confirmed in channel |
| 11 | Channel role allowlist: `ops` (can trigger) vs `observer` (read-only) |
| 12 | Bot tokens managed in Vault, rotated via `osagent rotate-channel-secret` |
| 13 | Channel outbox: SQLite per channel, replay on reconnect |
| 14 | Subagent depth: 1 (no grand-subagents) |
| 15 | Subagent cost: pool semantics (parent's daily cap is shared pool) |
| 16 | Subagent identity in audit: both parent + subagent (`engineer/secret-rotation-planner`) |
| 17 | Subagent prompt provenance: signed by wizard's git signature; engineer verifies before invoke |
| 18 | Subagent runtime isolation: separate Tokio task with own CancellationToken |
| 19 | Provider policy modes: `cloud-first` (default), `local-first`, `local-only` (hard wall — refuse to serve if oracle unreachable) |
| 20 | Default provider policy: `cloud-first` (most customers don't need high security) |
| 21 | Pause-gate semantics: CancellationToken into every tool; Vault writes complete current transaction then halt |
| 22 | Audit log sinks: journald + append-only file per customer, hash-chained, anchored daily to witness |
| 23 | Upstream subtree sync: quarterly, dedicated PR, "upstream-tag-N" CI integration test suite |
| 24 | Build-time MANIFEST.toml + `osagent manifest --diff config.toml` |
| 25 | CI gate: `nm osagent-wizard \| grep -i mcp` must be empty |
| 26 | Disaster recovery `osagent-rescue` CLI: same allowlist, no LLM, direct AMQP |
| 27 | Skill provenance signing: skill catalog signed; engineer verifies before load |
| 28 | Platform-skill / customer-override channel split with explicit precedence |
| 29 | Dashboard addressing: `?platform=<id>&agent=...` even though single-tenant |
| 30 | Production migration: sharp cutover (no coexist, no forward-only) |
| 31 | Binary naming: drop-in replace `/usr/local/bin/{engineer,wizard}` |
| 32 | Repo location: `andreas2301/osAgent` |
| 33 | Versioning: independent semver from `0.1.0+zeroclaw-0.7.5` build metadata |
| 34 | Telemetry strip: audit + remove all upstream phone-home |
| 35 | i18n: keep Mozilla Fluent pipeline, strip non-en locales |
| 36 | Approver-2 fallback: sysadmin SSH escape hatch via `osagent rescue approve <correlation_id>`; never auto-approve on timeout |
| 37 | Subagent definition format: markdown frontmatter (matches Claude Code convention) |
| 38 | Approval timeout: 1h, then escalate to sysadmin chat (no silent expiry) |
| 39 | Audit hash-chain anchor: daily to witness's chain (cross-link) |
| 40 | CI host: GitHub Actions public for PRs + self-hosted runner for signed releases |
| 41 | Telegram bot strategy: engineer + wizard each own one bot per customer (two tokens per customer) |
| 42 | Customer config: per-customer authoritative + read-only mirror in sovereign-shield-backup |

## Constraints

- **Upstream license**: zeroclaw is MIT OR Apache-2.0 — fork preserves both, adds osAgent NOTICE. Compatible with any closed-source customer deployment.
- **Compile-time wizard MCP exclusion**: the wizard binary's freestanding ELF symbols MUST NOT contain MCP code; CI enforces via `nm` grep gate. This is the load-bearing safety property and cannot be relaxed.
- **Drop-in to systemd**: binaries are named `engineer` and `wizard` (matching agent identities) so existing systemd units at `/etc/systemd/system/{engineer,wizard}.service` continue to work without unit edits.
- **Install-guide phase ordering preserved**: install-guide CLAUDE.md documents 14 invariant phase orderings, mTLS cert provisioning patterns, sandbox-allowed home-mirror, ExecStartPre preflight, AMQP env file. osAgent install ansible task must respect all of them.
- **No webhook ingress**: rejected by user on security grounds. Custom integration per weird-stack customer instead.
- **Drop the sandbox auto-detect chain**: zeroclaw's default Auto→Landlock→Firejail→Docker→Noop probe is what bit us in 2026-04-22 (Docker wrap broke engineer's bridge access). osAgent ships single `none` backend; config-load rejects `sandbox.enabled != false` unless build feature explicitly overrides.
- **No silent fallback in local-only mode**: airgap customers depend on the property that "local-only" means "no cloud traffic, period." osAgent emits an alert and refuses to serve when oracle is unreachable in local-only mode — never silent failover.
- **Single-tenant config invariant**: every path in config must start with `/opt/sovereign-shield/<customer_id>/` (asserted at config-load). Cross-customer data leakage at the config layer = refuse to start.
- **Plan-then-execute discipline preserved for install-guide work**: any phase that edits sovereign-shield-install-guide MUST emit a Touch/Change/Impact/Rollback plan before edits, per that repo's CLAUDE.md hard rule.

## Key Decisions

<!-- Decisions that constrain future work. The 42 decisions above are the comprehensive list; this table tracks outcomes as they materialize. -->

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Fork zeroclaw v0.7.5 rather than build from scratch | v0.7.5 has the `git -C` fix engineer depends on; >50% of code we keep is already there; quarterly upstream merge gives ongoing security fixes "for free" | — Pending |
| Two binaries via Cargo features in one workspace | Compile-time MCP exclusion on wizard provable by `nm`; single-repo development; shared crates avoid drift | — Pending |
| Drop webhook channel entirely | User: "rather build a custom osAgent update for a weird stack than overengineer and give the capabilities to get hacked" — webhooks are a generic ingress that's hard to harden | — Pending |
| MCP only on engineer binary | Wizard handles Vault writes; MCP servers are arbitrary-code-execution surfaces that could exfiltrate secrets even if benign-looking | — Pending |
| Engineer's bridge becomes a native Rust tool (not shell-invoked) | Today engineer-amqp-bridge runs via shell tool with AMQP env vars passed through; native tool eliminates the bash+python3+bridge shell path entirely, shrinking allowed_commands | — Pending |
| Audit log dual-sink + hash-chain | Journald rotation lost lines historically; append-only file gives witness reliable scrape source; hash-chain makes tampering detectable | — Pending |
| `cloud-first` as default provider policy | Most customers don't need high-security airgap; making the privacy/cost-conscious modes (`local-first`/`local-only`) opt-in matches typical customer profile | — Pending |
| Production migration via sharp cutover | Coexist doubles maintenance forever; forward-only strands existing installs; sharp cutover bundles the upgrade with the next ansible apply | — Pending |
| Documentation lands in sovereign-shield-backup | That repo already uses Arc42 numbering; osAgent docs slot in as `documentation/osAgent/` subdirectory; matches the existing pattern | — Pending |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd:transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd:complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-06-12 after initialization*
