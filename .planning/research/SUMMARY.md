# Project Research Summary

**Project:** osAgent (tailored fork of zeroclaw v0.7.5)
**Domain:** Two-binary Rust Cargo-workspace fork producing engineer + wizard agents for the sovereign-shield self-hosted single-tenant security platform
**Researched:** 2026-06-12
**Confidence:** HIGH (workspace mechanics, Cargo feature unification, install-guide carry-forward invariants); MEDIUM (exact upstream module names — verified during Phase 1 inventory); LOW (Signal channel integration path — see refinement #3 below)

---

## Executive Summary

osAgent is a **shape-not-behavior** fork. M1 strips zeroclaw v0.7.5 to a provably-small attack surface (6 channels, 5 providers, ~25 tools, gateway-`/ws/chat`-only) and restructures it into two compile-time-separated binaries — `engineer` (MCP-allowed, AMQP bridge to operator) and `wizard` (provably MCP-free, direct Vault writer). The load-bearing safety property is that wizard's ELF cannot contain MCP code, enforced by CI and the architecture pattern of structural crate-level exclusion. M2 adds engineer behavior change (native AMQP bridge, sqlcipher memory, hash-chain audit, lifecycle gates); M3 fills wizard logic and 2-of-2 Vault approval; M4 ships WhatsApp+Signal runtime and the wizard install cutover.

The recommended approach is **delete-don't-feature-gate** for high-risk capabilities (webhooks, MCP-on-wizard, sandbox auto-detect) and **separate-crate-don't-feature-flag** for the MCP boundary. Cargo resolver=2 silently unifies features across workspace members, so any pattern that puts MCP in a shared crate behind a feature flag will leak the symbols into wizard during `cargo build --workspace`. The architecture research's headline finding (Pitfall 1 + Anti-Pattern 1) supersedes PROJECT.md decision #1's "via Cargo features" framing — see Cross-Cutting Refinement #1 below.

The three highest-risk integrations for M1+M2 are (a) AMQP-mTLS dialing 127.0.0.1 with a `shield.internal` SAN cert (requires manual `tokio-rustls` stream construction, only `lapin` supports this — `amqprs` cannot), (b) the wizard MCP-exclusion CI gate must be **4-layer not just `nm`** (source-grep + `nm --defined-only` + `cargo-bloat --crates` + `strings`), and (c) Signal channel is **AGPL-contaminated in every Rust SDK** (presage, libsignal-service, libsignal) — Signal must run as out-of-process `signal-cli` daemon talking JSON-RPC over a Unix socket. Each of these refines PROJECT.md decisions and is called out below before any roadmap work.

---

## Cross-Cutting Refinements to PROJECT.md Decisions

These three findings emerged across multiple research files and **refine** (do not contradict) the 42 ratified decisions. They must propagate into REQUIREMENTS.md and ROADMAP.md so M1 phase structure encodes them correctly.

### Refinement 1 (Decision #1): MCP boundary is **structural crate exclusion**, not a Cargo feature

**Sources:** ARCHITECTURE.md headline finding; PITFALLS.md Pitfalls 1, 3, 6; Anti-Patterns 1+2.

**Finding:** PROJECT.md decision #1 says "Two binaries via Cargo features (`engineer-bin`, `wizard-bin`)." This is **insufficient.** Cargo resolver=2 unifies features across workspace members during `cargo build --workspace` (the workspace build that every dev and every CI run executes), silently relinking MCP into wizard. The CI gate would pass on isolated builds and fail on workspace builds — or, worse, pass everywhere and ship an MCP-tainted wizard because feature unification snuck the symbols back in.

**Correct pattern:** MCP lives in a separate crate `osagent-tools-mcp` (or `osagent-mcp`) that the wizard binary's `Cargo.toml` does NOT depend on at all. No feature flag. No optional dep. No `cfg` gate. **No dependency edge.** The engineer binary's `Cargo.toml` lists `osagent-tools-mcp` as a direct dep; the wizard binary's `Cargo.toml` does not. Adding/removing the dep is a single visible diff in code review. Pattern 2 in ARCHITECTURE.md ("Crate-Boundary Capability Exclusion"). The same pattern applies to `osagent-bridge` (engineer-only).

**Roadmap implication:** Phase 2 (workspace restructure) must establish this structural exclusion, not feature-flag exclusion. Phase 3 (MCP boundary + CI gate) is what validates it.

### Refinement 2 (Decision #25): CI gate is **4-layer**, not just `nm`

**Sources:** PITFALLS.md top-of-document answer #2; Pitfalls 1, 2, 3, 19.

**Finding:** PROJECT.md decision #25 specifies `nm osagent-wizard | grep -i mcp` must be empty. This is **necessary but not sufficient.** LTO can inline MCP-handling code into a parent function and erase the symbol; `strip = "symbols"` removes local symbols; non-deterministic DCE (rust #150462) makes CI passing non-binding for the customer-deployed binary; and string constants survive symbol stripping.

**Correct pattern:** four CI layers, all must pass:

| Layer | Check | Catches |
|-------|-------|---------|
| L1 | `grep -rnE '#\[cfg\(feature\s*=\s*"mcp"\)\]\|use .*mcp' crates/wizard-bin/ crates/wizard-*/` is empty | Source-level reachability to MCP |
| L2 | `nm -D --defined-only target/release/osagent-wizard \| grep -iE 'mcp\|model[_-]?context[_-]?protocol\|stdio_mcp\|sse_mcp'` is empty | Linked symbols (decision #25 baseline, but normalized) |
| L3 | `cargo bloat --release --bin osagent-wizard --crates \| grep -i mcp` is empty AND `cargo bloat --release --bin osagent-wizard --filter mcp -n 10` returns no rows | Inlined-into-parent symbols (post-LTO size attribution) |
| L4 | `strings target/release/osagent-wizard \| grep -Ei 'mcp\|stdio_mcp_server\|sse_mcp'` is empty | Log strings, serde tag literals, format strings |

Plus a **reproducibility CI lane** (two independent runs produce byte-identical binary) so the gate is binding on the customer-deployed artifact, not just on CI's runner.

**Roadmap implication:** Phase 3 wires all four layers. Phase 4 adds the reproducibility lane.

### Refinement 3 (Stack: Signal channel): Signal channel is **out-of-process subprocess**, not in-process Rust

**Sources:** STACK.md decision #9, Critical Integration Risk #3; PITFALLS.md security mistakes table.

**Finding:** All Rust Signal crates — `presage`, `libsignal-service`, `libsignal`, `libsignal-protocol`, `libsignal-client`, `libsignal-bridge` — are **AGPL-3.0**. A single transitive dep of any one of them forces the entire osAgent binary under AGPL, which breaks our MIT/Apache-2.0 dual license and would require publishing full source to every customer we ship to (a non-starter for closed-source customer deployments).

**Correct pattern:** Run `signal-cli` (GPLv3, FSF "mere aggregation" stance at the process boundary) as a **separate process** in JSON-RPC daemon mode. osAgent talks to it over a Unix-domain socket. License boundary is the process edge. `cargo deny` explicitly bans the six AGPL Rust Signal crate names as a CI lint.

Cost: a JVM dependency for Signal-enabled customers (acceptable per-customer trade) and Signal channel work shifts from "Rust crate integration" to "subprocess management + IPC + Java install in the ansible task." This is **higher complexity, lower license risk**, and must be reflected in M4's Signal-channel phase scope.

**Roadmap implication:** Signal channel work in M4 is materially larger than the other 5 channels and depends on Java/JVM provisioning in the install-guide. Flag M4 Signal as needing its own research phase. The `cargo deny` ban entries land in Phase 4 (whole-crate drops + license CI).

---

## Key Findings

### Recommended Stack

The base stack is **inherited from zeroclaw v0.7.5** and does not need to be redecided: tokio 1.52, axum 0.8, reqwest 0.13, serde, toml_edit, clap, tracing — all MIT/Apache-2.0. Nine net-new picks for osAgent (detailed in STACK.md), six of which are HIGH-confidence and three require care:

**Core net-new technologies:**
- **`lapin` 4.10** — AMQP client; chosen over `amqprs` because only lapin lets us inject a pre-built `tokio-rustls` stream with `ServerName` override (we dial 127.0.0.1 but the cert SAN is `shield.internal`)
- **`rusqlite` 0.40 with `bundled-sqlcipher-vendored-openssl`** + `tokio-rusqlite` 0.7 — SQLite-with-encryption, vendored OpenSSL avoids host-ABI roulette
- **`vaultrs` 0.8** — Vault KV v2 + AppRole, async; clean wrap point for idempotency-key middleware (decision #8)
- **`tokio-rustls` 0.26 + `rustls` 0.23** (pin `ring` provider, NOT `aws-lc-rs` — zeroclaw v0.7.5 already worked around the `aws-lc-rs` `.eh_frame` strip bug)
- **`gray_matter` 0.3** — markdown frontmatter parser for Claude-Code-style subagent definitions (decision #37)
- **`ssh-key` 0.6** for git-SSH-signed commit verification + **`ed25519-dalek` 2.2** for raw Ed25519 (skill provenance #17, #27)
- **`teloxide` 0.17** (Telegram), **`slack-morphism` 2.22** (Slack), **`matrix-sdk` 0.18** (Matrix) — all permissively licensed, actively maintained
- **In-house `reqwest` wrappers** for Mattermost (all four Rust crates abandoned) and WhatsApp-Cloud (`whatsapp-cloud-api` 0.5.4 exists but is 0% documented)
- **Out-of-process `signal-cli` daemon** (NOT presage/libsignal-service) — see Refinement #3

**Stack do-not-use list (license/safety bans):**
- `presage`, `libsignal-service`, `libsignal*` — AGPL contamination
- `frankenstein` (Telegram) — WTFPL, flagged by enterprise SBOM scanners
- `aws-lc-rs` rustls provider — `.eh_frame` strip incompatibility, preserve zeroclaw's `ring` pin
- `amqprs` — TLS wired through URI, cannot inject custom `ServerName`
- sandbox auto-detect chain (Auto→Landlock→Firejail→Docker→Noop) — 2026-04-22 incident
- Extism (WASM plugins), Qdrant/Postgres memory backends, `git2` libgit2 — out of scope per PROJECT.md strip targets

### Expected Features (M1 only)

**Must have (table stakes — all 13 M1 requirements):** Every M1 Active requirement (FORK-01, FORK-02, WS-01, WS-02, STRIP-01 through STRIP-06, TELEMETRY-01, MANIFEST-01, INSTALL-01) is P1. There is no smaller M1.

- **FORK-01/FORK-02** — public fork on andreas2301/osAgent with preserved attribution, osagent-main working branch, quarterly upstream-sync runbook documented at sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md
- **WS-01/WS-02** — two-binary workspace, MCP-exclusion CI gate (4-layer per Refinement #2)
- **STRIP-01 through STRIP-06** — whole-crate drops (zeroclaw-hardware, robot-kit, aardvark-sys, apps/tauri, zeroclaw-plugins), channel strip to 6, provider strip to 5, tool strip (~35 dropped), gateway strip with /ws/chat kept, webhook explicitly NOT included
- **TELEMETRY-01** — audit + strip all upstream phone-home; findings in TELEMETRY_AUDIT.md
- **MANIFEST-01** — build emits MANIFEST.toml from CARGO_FEATURE_* env vars + post-link symbol analysis (the [declared] + [detected] split, see Pitfall 8)
- **INSTALL-01** — drop-in install_osagent.yml ansible task (engineer-only at M1; wizard ansible lands in M3)

**Differentiators (sustainability):** quarterly upstream-sync runbook with diff-stat budget; manifest-diff CLI (osagent manifest --diff); en-only Fluent strip (keeps the pipeline); independent semver with 0.1.0+zeroclaw-0.7.5 build metadata.

**Anti-features (permanent prohibitions, document so future sessions do not re-add):**
- Webhook ingress channel (security)
- MCP on wizard (load-bearing safety property)
- Multi-tenancy at config layer (single-tenant invariant)
- Sandbox auto-detect chain (2026-04-22 incident)
- Silent failover in local-only provider policy (airgap promise)
- Outbound webhook subscriptions (exfiltration channel)
- Public artifact distribution (release engineering not stood up)
- Coexist forever (sharp cutover, decision #30)
- Subagent depth greater than 1 (fork-bomb hazard)
- Embedded web dashboard / pairing UI / mTLS server option on gateway (OS-MDashboard is canonical UI)

**Defer (v2+):** Microsoft Teams channel, APAC channels (Lark/WeCom/DingTalk/WeChat/QQ/LINE), Custom Landlock sandbox, public artifact distribution, re-add non-en locales.

### Architecture Approach

osAgent restructures zeroclaw single workspace into ~20 crates organized under crates/ plus two thin (~50 LOC) main.rs binary wrappers under bins/osagent-engineer/ and bins/osagent-wizard/. The binary crates Cargo.toml files are the **explicit, human-readable manifest of what is compiled into each binary** — every diff to them is a security-relevant code review.

**Major components:**

1. **osagent-engineer-bin / osagent-wizard-bin** — top-level binary crates; thin main.rs wiring shared crates + (engineer-only) osagent-tools-mcp + osagent-bridge
2. **osagent-tools-mcp** — isolated MCP crate, depended on by engineer-bin ONLY (the structural exclusion per Refinement #1)
3. **osagent-bridge** — native Rust AMQP+mTLS+operator-allowlist tool, engineer-only (replaces shell to bash to python3 to bridge chain)
4. **osagent-gateway-ws-only** — forked from zeroclaw-gateway with REST/SSE/ACP/dashboard/webhook/mTLS-server **source-deleted** (Pattern 4); only /ws/chat + paired_tokens auth survives
5. **osagent-exchange** — file-based PLAN/MISSION/REPORT bus under /var/lib/sovereign-shield/exchange/; both binaries read+write
6. **osagent-lifecycle + osagent-audit + osagent-subagent** — shared crates for CancellationToken propagation, hash-chained dual-sink audit, depth=1 subagent primitive with parent+sub identity in audit lines
7. **osagent-amqp** — shared TLS-bootstrap helper crate (Pitfall 14 enforcement: one helper, no per-service re-implementation)
8. **Other shared crates** — osagent-config, osagent-runtime, osagent-providers, osagent-memory, osagent-channels, osagent-tools-core, osagent-api, osagent-infra, osagent-macros, xtask

**Five architectural patterns:**

1. **Explicit-Registration Trait Registry** — main.rs lists every channel/provider/tool with registry.register(Box::new(...)) lines. No inventory::submit!, no linkme, no ctor (auto-discovery defeats the boundary).
2. **Crate-Boundary Capability Exclusion** — security-critical capabilities live in their own crate; the binary that must not have them does not list them as dependencies. Pattern is defeat-proof against feature unification.
3. **Top-Level Binary Crate as Manifest** — binaries are ~50 LOC main.rs wrappers; the binary crate Cargo.toml IS the security-review surface.
4. **Source-Level Strip (Not Feature-Gate) for High-Risk Removals** — webhooks, ACP, REST endpoints, plugins are rm -rfed, not cfged out. Deleted code cannot accidentally compile.
5. **Quarterly Upstream Merge via Subtree Replay** — git subtree merge, dedicated PR with conflict-resolution log, upstream-tag-N CI integration suite must include the 4-layer MCP gate.

### Critical Pitfalls

The PITFALLS.md research identifies 24 pitfalls; the top 5 by impact are:

1. **Pitfall 1 — Cargo workspace feature unification silently re-enables MCP on wizard.** Resolver=2 unifies features across workspace members. --no-default-features does not propagate to transitive deps. Mitigation: structural crate exclusion (Refinement #1) + workspace resolver = "2" pin + default-features = false on every shared crate + 4-layer CI gate (Refinement #2).

2. **Pitfall 3 — nm symbol-grep passes but code is inlined / monomorphized into the binary.** LTO erases symbol names; strip = "symbols" removes local symbols; trait-object registry casts instantiate vtables regardless of cfg gates. Mitigation: the 4-layer gate (Refinement #2) — nm alone is insufficient.

3. **Pitfall 7 — Quarterly upstream merge degenerates ("conflict fatigue").** Q1 clean; Q4 catastrophic. Mitigation: diff-stat budget (greater than 2000 LOC in kept crates = mandatory per-file review), UPSTREAM_SYNC.md conflict log, upstream-tag-N CI suite with full 4-layer MCP gate, subtree (not submodule) merge strategy.

4. **Pitfall 8 — MANIFEST.toml lies (build-time vs runtime drift).** If MANIFEST is generated by parsing Cargo.toml ("what we wished") instead of from CARGO_FEATURE_* + post-link analysis ("what rustc actually compiled in"), it gives operators false security. Mitigation: build.rs emits MANIFEST with both [declared] (Cargo features enabled) and [detected] (post-link symbol-set summary) sections; install-guide preflight runs osagent manifest --diff BIDIRECTIONALLY (refuses install if binary has features config did not authorize, not just if config asks for features binary lacks).

5. **Pitfall 20 — Telemetry strip incomplete (outbound metrics survive in transitive deps).** TELEMETRY-01 catches obvious posthog.com/sentry.io but misses crash-report defaults in transitive deps and DNS-leak telemetry. Mitigation: cargo tree -e normal dependency-graph audit; network-egress whitelist enforcement via iptables/nftables in the ansible install task; CI test under unshare -n (network namespace) to assert no startup network requirement.

**Carry-forward pitfalls from sovereign-shield-install-guide CLAUDE.md (lived experience):** class #2 (daemon-reload + restart on EnvironmentFile change), class #5 (invisible-on-rerun — clean-VM CI test mandatory), class #7 (live-vs-repo drift), class #10 (upstream-pin drift — get_url + checksum: only), class #11 + #12 (workbench ownership + cross-UID drift — bridge tool must have no CAP_CHOWN), class #13 (no dockerd restart in osAgent task), class #15 (StartLimitIntervalSec on engineer/wizard units), class #16 + #17 (no embedded credentials, no engineer-writes-life-config tool), class #18 (shared osagent-amqp crate for TLS), class #21 (TLS listener race retry budget), class #22 (pre-create audit log file for non-root daemons).

---

## Implications for Roadmap

Based on combined research, M1 is best structured as **6 sequential phases** matching the architecture research recommended build order. The order is dictated by hard dependencies between strips and restructures — e.g., explicit-registration migration must come before any channel/provider/tool source-deletion, and the MCP boundary + CI gate must land before any other strip so subsequent strips cannot regress it.

### Phase 1.1: Fork + Attribution + Upstream-Sync Runbook
**Rationale:** Precondition for everything. Without preserved attribution, M1 is non-distributable; without a written runbook, the fork rots within a quarter.
**Delivers:** Public andreas2301/osAgent fork; osagent-main working branch; LICENSE-APACHE, LICENSE-MIT, upstream NOTICE retained + osAgent NOTICE added; subtree (not submodule) remote configured; UPSTREAM_SYNC.md runbook with diff-stat budget, conflict-resolution log table, upstream-tag-N CI suite design, refuse-to-merge criteria (Pitfall 7).
**Addresses:** FORK-01, FORK-02.
**Avoids:** Pitfall 4 (upstream pin-drift), Pitfall 7 (merge degeneration), Pitfall 23 (license attribution drift).
**Establishes:** cargo about generate for NOTICE auto-aggregation + cargo deny check licenses allowlist (MIT, Apache-2.0, BSD-3, ISC, MPL-2.0, CC0-1.0).

### Phase 1.2: Workspace Skeleton + Binary Split + Read-Only Inventory
**Rationale:** Establishes the two-binary topology BEFORE any strip, so strips happen via Cargo.toml edits + source deletion against the new structure. Read-only inventory first because architecture research has MEDIUM confidence on exact upstream module names — Phase 1 confirms.
**Delivers:** Read-only audit of upstream module names (MCP locations, channel registration pattern — inventory::submit! vs explicit, gateway REST/WS split, telemetry call sites); rename zeroclaw-* to osagent-* (mechanical sed pass); create bins/osagent-engineer/ + bins/osagent-wizard/ top-level crates; migrate to Pattern 1 (explicit registration) for channels/providers/tools; establish shared osagent-amqp crate with TLS-bootstrap helper (Pitfall 14); pin resolver = "2", edition; enable [lints.rust.unexpected_cfgs] workspace-wide (Pitfall 2 + 18); pin default-features = false on every shared dep (Pitfall 1).
**Addresses:** WS-01.
**Avoids:** Pitfall 6 (wizard inherits engineer surface), Pitfall 11 (bridge with CAP_CHOWN — source lint), Pitfall 14 (TLS pattern duplication), Pitfall 18 (resolver shift), Pitfall 22 (engineer life-config write).
**Uses:** lapin, tokio-rustls, rustls (ring provider), rustls-pemfile.

### Phase 1.3: MCP Boundary + 4-Layer Wizard CI Gate
**Rationale:** The load-bearing safety property. Lands BEFORE other strips so subsequent strips cannot regress it. Gives the team a concrete green-check milestone early.
**Delivers:** Extract crates/osagent-tools-mcp/ as a separate crate (NOT a feature flag of shared tools); remove osagent-tools-mcp from bins/osagent-wizard/Cargo.toml; add .github/workflows/ci.yml with the 4-layer gate (L1 source-grep + L2 nm --defined-only + L3 cargo bloat --crates + L4 strings) running on BOTH isolated cargo build -p and workspace cargo build --workspace; configure release-build profile with CARGO_INCREMENTAL=0, codegen-units = 1, lto = "fat", strip = "none" for the gate then a separate strip pass for the deploy artifact (Pitfall 19).
**Addresses:** WS-02 (decision #25 with the 4-layer refinement).
**Avoids:** Pitfalls 1, 2, 3, 6, 19 — every Cargo-feature-unification leak path.
**This is the milestone-defining green check.** Until this passes, M1 is unfinished.

### Phase 1.4: Whole-Crate Drops + Telemetry Audit (Parallel-Safe Strips)
**Rationale:** Dropping 5 entire crates is mechanically simple and shrinks the surface fast. Telemetry audit runs in parallel because it does not touch workspace structure (it greps for outbound HTTP).
**Delivers:** Delete zeroclaw-hardware, robot-kit, aardvark-sys, apps/tauri, zeroclaw-plugins from workspace members; delete webhook channel source (STRIP-06); telemetry audit — cargo tree -e normal lists every HTTP client crate, each gets a justification entry in TELEMETRY_AUDIT.md; strip all phone-home (Sentry, PostHog, Honeycomb, vendor analytics, anonymous crash reports in transitive defaults); migrate any remaining bare-name optional deps to dep: prefix (RFC 3491, Pitfall 17); add cargo deny ban entries for AGPL Signal crates + WTFPL frankenstein.
**Addresses:** STRIP-01, STRIP-06, TELEMETRY-01.
**Avoids:** Pitfall 17 (implicit features deprecation), Pitfall 20 (telemetry survival in transitive deps).

### Phase 1.5: Channel/Provider/Tool Source Strips + MANIFEST.toml
**Rationale:** Now that Pattern 1 (explicit registration) is in place from Phase 1.2 and dead crates are gone from Phase 1.4, channel/provider/tool deletion is rm -rf + remove registry.register(...) line. MANIFEST emission lands here because it describes the post-strip outcome.
**Delivers:** Delete 24 channel impls keeping Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal; delete ~55 provider impls keeping Anthropic, Gemini, Kimi (via openai-compatible), Ollama, OpenRouter; delete ~35 tool impls; strip memory backends (qdrant, postgres, embeddings, consolidation, community-skill HTTP); strip non-en Fluent locales (keep the pipeline); build.rs-emitted MANIFEST.toml with [declared] (from CARGO_FEATURE_*) AND [detected] (post-link symbol-set summary) sections, SHA256-hashed; osagent manifest --diff config.toml CLI command (bidirectional check); reproducibility CI lane (two independent runs produce byte-identical binary, Pitfall 19); version metadata 0.1.0+zeroclaw-0.7.5.
**Addresses:** STRIP-02, STRIP-03, STRIP-04, MANIFEST-01.
**Avoids:** Pitfall 8 (MANIFEST lies), Pitfall 19 (non-deterministic DCE).

### Phase 1.6: Gateway Fork + Install Ansible Drop-In
**Rationale:** Last because install ansible touches sovereign-shield-install-guide, which has plan-then-execute discipline and 14 invariant phase orderings (preserved per project memory). Ship when everything else is stable so the install plan reflects the final binary shape.
**Delivers:** Copy zeroclaw-gateway to new crates/osagent-gateway-ws-only/, source-delete REST endpoints (/config, /onboarding, /pairing, /personality, /plugins, /webauthn), ACP bridge, SSE, embedded web dashboard, pairing dashboard UI, mTLS server option, outbound webhooks; keep /ws/chat + paired_tokens auth (OS-MDashboard chat-relay.ts is load-bearing); remove zeroclaw-gateway from workspace; sovereign-shield-install-guide/ansible/install_osagent.yml as drop-in replacement for install_zeroclaw.yml covering engineer only (wizard install lands in M3); systemd unit names match existing (engineer.service); env file at /etc/sovereign-shield/osagent-engineer.env (mode 0640 root:engineer); pre-create audit log at /var/log/sovereign-shield/osagent-audit-CUSTOMERID.log with state=touch modification_time=preserve (Pitfall 16); StartLimitIntervalSec=60 StartLimitBurst=3 on the systemd unit (Pitfall 15); handler chain for daemon-reload + restart on env-file change (Pitfall 9); cert mirror to HOME/.zeroclaw/certs/engineer-amqp/ (Pitfall 14 sandbox-allowed); clean-VM CI test against fresh ubuntu:24.04 container (Pitfall 10); get_url + checksum: sha256: for binary install (Pitfall 4); osagent --version post-install assertion of binary SHA + MANIFEST hash.
**Addresses:** STRIP-05, INSTALL-01.
**Avoids:** Pitfalls 4, 5, 9, 10, 11, 12, 13, 14, 15, 16, 21, 22 — the entire install-guide carry-forward set.
**Process constraint:** plan-then-execute (Touch/Change/Impact/Rollback) before any install-guide edit, per that repo CLAUDE.md hard rule.

### Phase Ordering Rationale

- 1.1 before everything: legal precondition; cannot distribute without attribution. Subtree-vs-submodule decision must be set before any merge.
- 1.2 before 1.3: need the workspace topology + explicit-registration pattern before extracting MCP as a separate crate.
- 1.3 before 1.4/1.5/1.6: the gate is the load-bearing safety property. Wiring it AFTER strips means strips might leave MCP references in shared crates that the gate then catches as regressions to fix retroactively.
- 1.4 before 1.5: dropping whole crates is mechanically simple; doing it first lets file-level strips work on a smaller codebase. Telemetry audit parallels 1.4 because it is grep-not-restructure.
- 1.5 before 1.6: MANIFEST.toml emission must reflect the post-strip surface, not an intermediate state. The install ansible task references the MANIFEST hash for post-install assertion.
- 1.6 last: install-guide work has the plan-then-execute discipline gate; ship when binary shape is final.

### Research Flags

**Phases likely needing deeper research during planning:**

- Phase 1.2 (read-only inventory): MEDIUM confidence on exact upstream module names. Phase 1 inventory step IS that research; if upstream is already explicit-registration, 1.2 is cheap; if it is auto-discovery, 1.2 is the most labor-intensive phase. Confirm with /gsd:research-phase if inventory surfaces surprises.
- Phase 1.3 (MCP boundary): LOW research need, but HIGH execution care — the 4-layer gate wiring is novel and cargo-bloat post-LTO attribution behavior across glibc/musl needs verification on the target runners.
- Phase 1.6 (install ansible): install-guide CLAUDE.md invariants are LIVED experience but extending them to osAgent paths needs research-phase confirmation against current install-guide state.

**Forward-looking research flags (M2/M3/M4):**

- M2 native bridge tool: AMQP-mTLS with ServerName differing from dial address (Critical Integration Risk #1 in STACK.md) needs an integration test against a test-CA-signed RMQ; research the lapin Connection::connect_with_stream exact API in 4.10.
- M2 sqlcipher memory backend: tokio-rusqlite 0.7 vs rusqlite 0.40 semver compatibility is an open question (STACK.md Risk #2) — research-phase confirms before writing code.
- M3 wizard 2-of-2 Vault approval UX: approval-flow patterns across dashboard + chat with distinct-identity verification (decision #5) needs UX + protocol research.
- M3 subagent depth=1 with pool-cost arithmetic + signed provenance: novel composition; research existing patterns.
- M4 Signal channel via signal-cli JSON-RPC bridge: SUBSTANTIAL research needed — subprocess management, Unix socket protocol, Java install in ansible, AGPL boundary documentation. Material complexity bump from the other 5 channels.
- M4 oracle routing + provider policy modes: type-level separation of LocalOnlyProvider vs FallbackProvider (Pitfall 24) needs design research.

**Phases with standard patterns (skip research-phase if appropriate):**

- Phase 1.4 (whole-crate drops): mechanical rm -rf + members edits.
- Phase 1.5 (file-level strips): mechanical given Pattern 1 is in place.

---

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | Inherited core stack is zeroclaw v0.7.5 (already pinned); 9 net-new picks verified against docs.rs current versions, license types, and ecosystem maturity. MEDIUM only on Mattermost/WhatsApp-Cloud (in-house wrapper path chosen because Rust crates are abandoned/undocumented). LOW on Signal (AGPL contamination forces subprocess pattern — Refinement #3). |
| Features | HIGH | M1 scope is fully determined by 42 ratified PROJECT.md decisions + explicit Active requirement list. No open-ended ecosystem survey needed; categorization (table-stakes / differentiator / anti-feature) is deterministic. |
| Architecture | HIGH | On the feature-unification pitfall + separate-crate solution (RFC 3692, Cargo book, multiple independent sources). HIGH on Pattern 1 (explicit registration) for capability-bounded binaries. MEDIUM on exact upstream module names (Phase 1.2 inventory confirms). MEDIUM on whether zeroclaw uses inventory/linkme — material impact on Phase 1.2 effort. |
| Pitfalls | HIGH | On Cargo invariants (Pitfalls 1-3, 17-19 — official-doc-anchored) and on install-guide carry-forwards (Pitfalls 4-5, 9-16, 21-22 — lived experience). MEDIUM on Pitfalls 6, 19 (trait-object monomorphization leak path is mechanically clear but needs verification once 1.2 workspace structure is concrete). MEDIUM/LOW on Pitfall 24 (provider-policy specifics are M2 forward-looking). |

**Overall confidence:** HIGH for M1 scope; MEDIUM-to-HIGH for M2; MEDIUM for M3/M4 (Signal subprocess, oracle routing, subagent composition need their own research phases when those milestones begin).

### Gaps to Address

- Exact upstream module names for MCP code, channel registration mechanism, gateway REST/WS module split. How to handle: Phase 1.2 read-only inventory IS the resolution; document findings in the planning notes before any rename pass.
- tokio-rusqlite 0.7 vs rusqlite 0.40 semver compatibility (STACK.md Risk #2). How to handle: confirm on first cargo check in Phase 1.2 workspace skeleton; document pinned versions in Cargo.toml comments. Three outcomes documented in STACK.md; all are tractable.
- vaultrs 0.8 reqwest version (0.12 vs 0.13). How to handle: acceptable risk; mismatch causes second reqwest compilation but not a correctness issue. Flag for M2 when wizard Vault writer lands.
- slack-morphism 2.22 axum 0.8 vs 0.7 compatibility. How to handle: verify on first build in Phase 1.5; if still on 0.7, wrap slack-morphism hyper router separately (small operational overhead).
- Per-customer egress whitelist enforcement (Pitfall 20) — iptables/nftables rules in the ansible install task need design. How to handle: Phase 1.6 design; M2 implements full enforcement once provider chain is in place.
- Java/JVM provisioning for Signal customers (Refinement #3) — install-guide needs a Java install task for Signal-enabled customers only. How to handle: flag for M4 planning; defer the question to that milestone research phase.
- Cross-cutting M2 architectural constraint: local-only provider policy uses a different type (no fallback method) than cloud-first/local-first. How to handle: document the type-level separation in Phase 1.2 architecture notes so the M2 provider crate respects it.

---

## Sources

### Primary (HIGH confidence)

- d:/Repositories/osAgent/.planning/PROJECT.md — 42 ratified architectural decisions, constraints, M1 Active requirements (user-ratified, not researched)
- d:/Repositories/osAgent/.planning/research/STACK.md — net-new crate picks with version/license/rationale; alternatives considered; integration risks
- d:/Repositories/osAgent/.planning/research/FEATURES.md — M1 13-requirement scope, table-stakes vs differentiator vs anti-feature categorization, dependency graph
- d:/Repositories/osAgent/.planning/research/ARCHITECTURE.md — workspace layout, MCP boundary structural enforcement, 5 architectural patterns, 5 anti-patterns, build matrix, 6-phase build order
- d:/Repositories/osAgent/.planning/research/PITFALLS.md — 24 pitfalls with prevention strategies, pitfall-to-phase mapping, looks-done-but-isnt checklist, recovery strategies
- RFC 3692 Feature Unification (https://rust-lang.github.io/rfcs/3692-feature-unification.html) — workspace-level feature unification semantics (Refinement #1)
- Cargo Book Features (https://doc.rust-lang.org/cargo/reference/features.html) — resolver = "2" behavior
- Cargo Workspace and the Feature Unification Pitfall, nickb.dev (https://nickb.dev/blog/cargo-workspace-and-the-feature-unification-pitfall/) — primary reference for Pitfall 1 / Refinement #1
- libsignal GitHub (https://github.com/signalapp/libsignal) — confirmed AGPL-3.0, use-outside-of-Signal-unsupported (Refinement #3)
- presage GitHub (https://github.com/whisperfish/presage) — confirmed AGPL-3.0 v0.7.0 (Refinement #3)
- signal-cli GitHub (https://github.com/AsamK/signal-cli) — confirmed GPLv3, JSON-RPC daemon mode (Refinement #3)
- sovereign-shield-install-guide/CLAUDE.md — 14 invariant phase orderings, anti-pattern classes 2, 5, 7, 10, 11, 12, 13, 15, 16, 17, 18, 21, 22

### Secondary (MEDIUM confidence)

- Cargo issue 1886 / 8366 / 11329 (https://github.com/rust-lang/cargo/issues/1886) — no-default-features does not propagate to transitive deps
- Rust issue 150462 (https://github.com/rust-lang/rust/issues/150462) — non-deterministic dead-code elimination (Pitfall 19)
- RFC 3491 remove implicit features (https://rust-lang.github.io/rfcs/3491-remove-implicit-features.html) — dep prefix migration (Pitfall 17)
- Cargo issue 14774 (https://github.com/rust-lang/cargo/issues/14774) — workspace feature-unification tracking (Pitfall 18)
- RFC 3013 check-cfg (https://rust-lang.github.io/rfcs/3013-conditional-compilation-checking.html) — Pitfall 2
- GitHub Docs About Git subtree merges (https://docs.github.com/en/get-started/using-git/about-git-subtree-merges) — Pitfall 7
- docs.rs version verification for lapin 4.10, rusqlite 0.40, vaultrs 0.8, gray_matter 0.3, ssh-key 0.6, ed25519-dalek 2.2, teloxide 0.17, slack-morphism 2.22, matrix-sdk 0.18 (STACK.md)

### Tertiary (LOW confidence — flagged for verification)

- Exact upstream zeroclaw module names for MCP (mcp_client, mcp_protocol, etc.) — Phase 1.2 inventory confirms
- Exact upstream channel registration mechanism (inventory/linkme vs explicit) — Phase 1.2 inventory confirms
- LOC count estimate (~60K lines for the strips) — prior-session context; not load-bearing
- tokio-rusqlite 0.7 vs rusqlite 0.40 semver compatibility — cargo check in Phase 1.2 confirms

---

*Research completed: 2026-06-12*
*Ready for roadmap: yes*
*Cross-cutting refinements to PROJECT.md flagged inline above; orchestrator must propagate Refinements #1, #2, #3 into REQUIREMENTS.md and ROADMAP.md.*
