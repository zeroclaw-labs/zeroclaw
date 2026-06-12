# Roadmap: osAgent — M1 (Foundation)

## Overview

osAgent is a tailored fork of zeroclaw v0.7.5 producing two compile-time-separated binaries (`engineer` and `wizard`) that drop into sovereign-shield's existing systemd units. **M1 — Foundation** ships a buildable, provably-shaped fork: public repo with attribution, two-binary workspace topology, structural MCP exclusion verified by a 4-layer CI gate, ~60K LOC of dead surface deleted (channels, providers, tools, gateway sub-surface, webhooks, telemetry), a build-time `MANIFEST.toml` that doesn't lie, and a drop-in ansible install task for the engineer binary. M1 is about **shape, not behavior** — engineer/wizard runtime feature changes (native AMQP bridge, sqlcipher memory, hash-chain audit, 2-person Vault approval, subagents, full channel runtimes) land in M2/M3/M4. The milestone-defining green check is Phase 1.3: the 4-layer wizard-no-MCP CI gate passing on every PR. The riskiest work is Phase 1.5 (24 named pitfalls concentrated there). The slow-burn risk is Phase 1.1 (fork rots without UPSTREAM_SYNC.md discipline).

## Milestones

- 🚧 **M1 — Foundation** — Phases 1.1–1.6 (in progress)
- 📋 **M2 — Engineer binary production-ready** — planned, not yet roadmapped
- 📋 **M3 — Wizard binary + subagent system** — planned, not yet roadmapped
- 📋 **M4 — Channels + Ops + Provider routing + Production rollout** — planned, not yet roadmapped

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (1.1, 1.2, ...): M1's six foundation sub-phases (executed in numeric order)

- [ ] **Phase 1.1: Fork & Attribution & Sync Runbook** — Public andreas2301/osAgent fork with preserved attribution, quarterly upstream-sync runbook, cargo-deny license + AGPL/WTFPL bans
- [ ] **Phase 1.2: Workspace Skeleton & Binary Split** — Two top-level binary crates, explicit-registration trait registries, read-only inventory of upstream module names, resolver=2 pin, default-features=false hygiene
- [ ] **Phase 1.3: MCP Boundary & 4-Layer CI Gate** — Structural `osagent-tools-mcp` crate exclusion, 4-layer CI gate (source-grep + nm + cargo-bloat + strings), reproducibility lane — MILESTONE-DEFINING GREEN CHECK
- [ ] **Phase 1.4: Whole-Crate Drops & Telemetry Audit** — Drop 5 dead crates, delete webhook channel source, audit + strip all phone-home, cargo-deny ban entries for AGPL Signal SDKs
- [ ] **Phase 1.5: Source Strips & MANIFEST Emission** — Strip 24 channels / ~55 providers / ~35 tools / non-en locales, build-time `MANIFEST.toml` with `[declared]+[detected]` sections, `osagent manifest --diff` CLI, reproducibility profile
- [ ] **Phase 1.6: Gateway Fork & Install Drop-In** — Forked `osagent-gateway-ws-only` (kept `/ws/chat` + paired_tokens), engineer-only `install_osagent.yml` ansible PR opened against sovereign-shield-install-guide

## Phase Details

### Phase 1.1: Fork & Attribution & Sync Runbook
**Goal**: Establish a legally-distributable, quarterly-sustainable fork with license discipline ratified at the CI layer.
**Depends on**: Nothing (first phase)
**Requirements**: FORK-01, FORK-02, FORK-03
**Success Criteria** (what must be TRUE):
  1. `andreas2301/osAgent` is public on GitHub, working branch is `osagent-main`, `git fetch upstream` works, `LICENSE-APACHE` + `LICENSE-MIT` + upstream `NOTICE` + osAgent `NOTICE` all present in repo root
  2. `sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md` exists and documents: quarterly cadence (Q1/Q2/Q3/Q4 first-week), diff-stat budget per merge, conflict-resolution log format, out-of-cycle critical-security-fix criteria, append-only conflict log
  3. `cargo deny check` passes in CI on every PR with license allowlist (MIT/Apache-2.0/BSD-2/BSD-3/ISC/Unicode-DFS-2016) and explicit bans for AGPL-3.0 crates (presage, libsignal-service, libsignal, libsignal-protocol, libsignal-client, libsignal-bridge) and WTFPL (frankenstein)
  4. Advisory database currency check (`cargo deny check advisories`) runs in CI and blocks merge on RUSTSEC matches
**Plans**: 4 plans
- [ ] 01.1-00-PLAN.md — Wave 0 preflight (tooling install, namespace-type discovery, phone-home grep, validation harness skeletons)
- [ ] 01.1-01-PLAN.md — GitHub fork + remotes + osagent-main + NOTICE + Cargo.toml metadata + branch protection [FORK-01]
- [ ] 01.1-02-PLAN.md — deny.toml + .github/workflows/ci.yml (SHA-pinned cargo-deny-action) + required-status-checks update [FORK-03]
- [ ] 01.1-03-PLAN.md — UPSTREAM_SYNC.md runbook on sovereign-shield-backup with Touch/Change/Impact/Rollback PR [FORK-02]

Notes:
- Slow-burn failure mode: without UPSTREAM_SYNC.md as binding ritual, the fork drifts within a quarter. Runbook discipline is what makes Q4 not catastrophic.
- The Signal SDK ban entries are M1-relevant (cargo-deny enforcement) even though Signal channel runtime is M4.

### Phase 1.2: Workspace Skeleton & Binary Split
**Goal**: Establish the two-binary workspace topology and explicit-registration pattern that all subsequent strips depend on.
**Depends on**: Phase 1.1
**Requirements**: WS-01, WS-04, WS-05
**Success Criteria** (what must be TRUE):
  1. `cargo build --workspace` succeeds with `resolver = "2"` pinned and `bins/osagent-engineer/` + `bins/osagent-wizard/` as top-level binary crates whose `Cargo.toml` files are the human-readable manifest of compiled-in capabilities
  2. Channels, providers, and tools are registered via explicit `registry.register(Box::new(Factory))` calls in each binary's `main.rs` — zero use of `inventory::submit!`, `linkme::distributed_slice`, or `ctor` anywhere in the workspace
  3. All workspace dependencies use `default-features = false` and explicit feature lists; `cargo tree --duplicates` is empty (run in CI)
  4. Read-only inventory of upstream module names is documented in `.planning/upstream-inventory.md`: MCP code locations, channel registration mechanism, gateway REST/WS module split, telemetry call sites
**Plans**: TBD

Notes:
- Establishes the shared `osagent-amqp` crate scaffold (Pitfall 14: one TLS-bootstrap helper, no per-service re-implementation) even though AMQP runtime is M2.
- Cross-cutting Refinement #1 propagates here: MCP boundary will be **structural crate exclusion**, not Cargo features. This phase prepares the topology; Phase 1.3 extracts the crate.

### Phase 1.3: MCP Boundary & 4-Layer CI Gate
**Goal**: Make the load-bearing safety property (wizard cannot exfiltrate Vault secrets via MCP) provable in CI on every PR — the milestone-defining green check.
**Depends on**: Phase 1.2
**Requirements**: WS-02, WS-03
**Success Criteria** (what must be TRUE):
  1. `osagent-tools-mcp` exists as a dedicated crate; `bins/wizard/Cargo.toml` has zero `mcp` references (no dependency edge, no feature flag, no `cfg` gate, no optional dep); `bins/engineer/Cargo.toml` explicitly depends on `osagent-tools-mcp`
  2. 4-layer CI gate runs on every PR and release build, all four layers must pass; any single failure breaks the build:
      - L1: `grep -rnE '#\[cfg\(feature\s*=\s*"mcp"\)\]|use .*mcp' bins/wizard/ crates/wizard-*/` is empty
      - L2: `nm --defined-only target/release/osagent-wizard | grep -iE 'mcp|model[_-]?context[_-]?protocol|stdio_mcp|sse_mcp'` is empty
      - L3: `cargo bloat --crates --release --bin osagent-wizard` does not list any `mcp` crate
      - L4: `strings target/release/osagent-wizard | grep -iE 'mcp|stdio_mcp_server|sse_mcp'` is empty
  3. The 4-layer gate runs against BOTH isolated `cargo build -p osagent-wizard` AND workspace `cargo build --workspace` builds (catches feature-unification regressions)
  4. CI passes green on a hollow-wizard-at-M1 scaffold (~50 LOC `main.rs`) — the gate is binding even before wizard runtime fills in at M3
**Plans**: TBD

Notes:
- **This is the milestone-defining gate.** M1 cannot be marked complete until the 4-layer CI gate is green.
- Cross-cutting Refinement #2 propagates here: gate is 4-layer (not single `nm` grep) because LTO inlines, `strip = "symbols"` removes locals, non-deterministic DCE (rust #150462) makes single-layer non-binding.
- Establishing the gate BEFORE strips means subsequent strips cannot regress the property — they have to keep CI green.

### Phase 1.4: Whole-Crate Drops & Telemetry Audit
**Goal**: Shrink the attack surface fast via mechanical whole-crate deletion and audit/remove all upstream phone-home.
**Depends on**: Phase 1.3
**Requirements**: STRIP-01, STRIP-06, TELEMETRY-01
**Success Criteria** (what must be TRUE):
  1. `zeroclaw-hardware`, `robot-kit`, `aardvark-sys`, `apps/tauri`, `zeroclaw-plugins` are physically deleted from the workspace; `cargo build --workspace` still succeeds; binary size and compile time both measurably drop
  2. Webhook channel source is physically deleted (not feature-gated); cargo-deny includes a check preventing re-introduction without a documented decision
  3. `docs/telemetry-audit.md` documents every outbound HTTP call site found in the codebase (`reqwest::Client::new`, `sentry::`, `posthog::`, `honeycomb::`, env-driven URLs like `SENTRY_DSN`/`POSTHOG_KEY`); all identified phone-home paths are removed at source level
  4. `cargo deny` configuration extended to ban sentry, posthog, honeycomb, opentelemetry-exporter-otlp-http, and any reqwest-based telemetry crates as direct deps; `cargo tree -e normal` audit confirms no transitive telemetry survives; CI test under `unshare -n` asserts no startup network requirement
  5. 4-layer wizard-no-MCP gate (Phase 1.3) still passes
**Plans**: TBD

Notes:
- Parallel-safe with channel/provider/tool strips conceptually, but sequenced after Phase 1.3 so the MCP gate is binding before any code deletion.
- Pitfall 20 (telemetry survival in transitive deps) is the highest hidden risk here.

### Phase 1.5: Source Strips & MANIFEST Emission
**Goal**: Reduce channels/providers/tools to v1 surface and produce a build-time `MANIFEST.toml` that does not lie about what is compiled in.
**Depends on**: Phase 1.4
**Requirements**: STRIP-02, STRIP-03, STRIP-04, STRIP-07, MANIFEST-01, MANIFEST-02, MANIFEST-03
**Success Criteria** (what must be TRUE):
  1. 26 channel implementations physically deleted; 6 keepers remain in source (Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal); ~50 provider implementations deleted, 5 keepers remain (Anthropic, Gemini, Kimi-code via OpenAI-compatible base, Ollama, OpenRouter); ~35 tool implementations deleted per STRIP-04 list; non-en Fluent locale files deleted, en-US `.ftl` files retained as authoritative source
  2. Build emits `MANIFEST.toml` next to each binary with two sections: `[declared]` derived from `CARGO_FEATURE_*` env vars + Cargo.toml dependency tree at build time, and `[detected]` derived from post-link symbol analysis (`cargo bloat --crates`); CI fails on `[declared] != [detected]` divergence (catches "feature declared but code orphaned" and "code linked but not declared")
  3. `osagent manifest --diff <config.toml>` CLI subcommand exists on both binaries and refuses-to-start on bidirectional mismatch (config asks for capability binary lacks OR binary contains capability config did not authorize)
  4. Reproducible-build profile pinned: `[profile.release]` sets `codegen-units = 1`, `lto = "fat"`, `strip = "symbols"`, `panic = "abort"`; CI builds with `CARGO_INCREMENTAL=0`; two-run byte-equality assertion passes in the release job (mitigates rust #150462 non-deterministic DCE)
  5. 4-layer wizard-no-MCP gate (Phase 1.3) still passes
**Plans**: TBD

Notes:
- **Highest-risk phase.** PITFALLS.md's 24 named pitfalls concentrate here — Pitfall 8 (MANIFEST lies), Pitfall 17 (implicit features deprecation / `dep:` prefix), Pitfall 19 (non-deterministic DCE), Pitfall 20 (telemetry transitive survival).
- Memory backend strip (qdrant, postgres, embeddings, consolidation, community-skill HTTP) folded into the tool/provider strip pass.
- Mechanically simple given Pattern 1 (explicit registration) is already in place from Phase 1.2: deletion is `rm -rf` + removing `registry.register(...)` lines.

### Phase 1.6: Gateway Fork & Install Drop-In
**Goal**: Fork the gateway crate to a minimal `/ws/chat`-only surface and open a drop-in ansible install task PR against sovereign-shield-install-guide.
**Depends on**: Phase 1.5
**Requirements**: STRIP-05, INSTALL-01
**Success Criteria** (what must be TRUE):
  1. `crates/osagent-gateway-ws-only/` exists as a forked-from-zeroclaw-gateway crate with REST endpoints (`/config`, `/onboarding`, `/pairing`, `/personality`, `/plugins`, `/webauthn`), ACP bridge, SSE, embedded web dashboard, pairing dashboard UI, mTLS server option, and outbound webhook endpoints all physically source-deleted; `/ws/chat` endpoint and `paired_tokens` auth path are kept and verifiably wire-compatible with OS-MDashboard's `chat-relay.ts`; old `zeroclaw-gateway` removed from workspace members
  2. PR opened against `sovereign-shield-install-guide` main containing `ansible/install_osagent.yml` as a structural template for the M2-completing engineer-binary install, NOT MERGED at M1 (merge happens at M2 close when engineer reaches parity)
  3. PR description contains a Touch/Change/Impact/Rollback plan posted BEFORE any ansible file is touched, per install-guide CLAUDE.md hard rule
  4. `install_osagent.yml` respects all 14 install-guide invariants: phase ordering preserved, mTLS cert provisioning pattern (sandbox-allowed home-mirror paths), `ExecStartPre` preflight, AMQP env-file pattern, pre-create audit log file before non-root daemon opens it, `StartLimitIntervalSec`/`StartLimitBurst` on systemd unit, `daemon-reload + restart` handler chain on env-file change, `get_url + checksum: sha256:` for binary install, clean-VM CI test against fresh `ubuntu:24.04` container
  5. 4-layer wizard-no-MCP gate (Phase 1.3) still passes; M1 ships
**Plans**: TBD

Notes:
- **ALL edits to sovereign-shield-install-guide require Touch/Change/Impact/Rollback plan in PR description before any file is touched**, per that repo's CLAUDE.md hard rule (plan-then-execute overrides auto-mode in install-guide work).
- Always cut a fresh branch off `master` of install-guide; stash unrelated dirty files first.
- Cross-UID ownership: bridge tool / install task must not chown into bind-mounted trees without explicit chown-back to outer-dir owner (Pitfall 12).
- PR is opened-not-merged because engineer binary is not yet at parity; merge is M2's exit gate.

## Progress

**Execution Order:**
Phases execute in numeric order: 1.1 → 1.2 → 1.3 → 1.4 → 1.5 → 1.6

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1.1 Fork & Attribution & Sync Runbook | 0/4 | Not started | - |
| 1.2 Workspace Skeleton & Binary Split | 0/TBD | Not started | - |
| 1.3 MCP Boundary & 4-Layer CI Gate | 0/TBD | Not started | - |
| 1.4 Whole-Crate Drops & Telemetry Audit | 0/TBD | Not started | - |
| 1.5 Source Strips & MANIFEST Emission | 0/TBD | Not started | - |
| 1.6 Gateway Fork & Install Drop-In | 0/TBD | Not started | - |

---

*Roadmap created: 2026-06-12*
*Granularity: standard (6 phases for M1, within 5-8 band)*
*M1 coverage: 20/20 v1 requirements mapped, 0 orphans*
*Phase 1.1 planned: 2026-06-12 — 4 plans, 2 waves (wave 0 preflight + wave 1 fork + wave 2 deny.toml/CI || UPSTREAM_SYNC.md)*
