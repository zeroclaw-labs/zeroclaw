# Feature Research — M1 Foundation

**Domain:** Rust agent harness fork (tailored downstream of zeroclaw v0.7.5)
**Researched:** 2026-06-12
**Confidence:** HIGH (scope is derived from 42 ratified decisions + explicit M1 requirement list in PROJECT.md, not from open-ended ecosystem survey)
**Scope:** M1 only — "buildable fork with our shape." Engineer/wizard runtime feature set (native bridge, exchange channel, lifecycle gates, sqlcipher, audit hash-chain, wizard binary, subagents) belongs to M2/M3/M4 and is excluded here.

---

## Definition of Done for M1

M1 ships when **all six conditions** are true simultaneously:

1. `andreas2301/osAgent` exists as a public fork of upstream with `osagent-main` branch and upstream attribution intact (FORK-01).
2. `cargo build --bin osagent-engineer --features engineer-bin` succeeds. (The wizard build target compiles as a hollow shell at M1; full wizard logic lands in M3, but the workspace topology must already support it so M3 is purely additive.)
3. `nm osagent-wizard | grep -i mcp` returns empty as a CI gate, even though the wizard binary at M1 is a minimal scaffold (WS-02). The gate must exist now so M3 cannot regress.
4. The five whole crates (`zeroclaw-hardware`, `robot-kit`, `aardvark-sys`, `apps/tauri`, `zeroclaw-plugins`) are gone from the workspace, plus the channel/provider/tool strip targets are met (STRIP-01 through STRIP-06).
5. Build produces a `MANIFEST.toml` next to each binary listing every compiled-in channel/provider/tool (MANIFEST-01).
6. `sovereign-shield-install-guide/ansible/install_osagent.yml` is a working drop-in replacement for `install_zeroclaw.yml` covering the engineer binary only (INSTALL-01).

M1 is **not** about behavior change — engineer still talks to Telegram/Slack via the same gateway code, still wraps shell in the same way. M1 is about **shape**: smaller surface, two-binary topology, verifiable manifest, owned install path. Behavior change is M2's job.

---

## Feature Landscape

### Table Stakes (M1 isn't actually a fork without these)

Features whose absence means "you didn't really fork it, you just renamed it" or "it doesn't actually build." Users (= sovereign-shield platform deployers) assume these exist before considering anything else.

| Feature | Why Table-Stakes for M1 | Complexity | Dependencies | M2+ Continuation |
|---------|------------------------|------------|--------------|------------------|
| **Public GitHub fork with preserved attribution** (FORK-01) | A fork without `LICENSE-APACHE` + `LICENSE-MIT` + upstream `NOTICE` retained is a license violation, not a fork. `osagent-main` branch isolates our work from upstream `main` so quarterly merges have a clean target. | S | None (precondition for everything else) | M2+ continues to land on `osagent-main`; upstream merges arrive as dedicated PRs (decision #23) |
| **Two-binary Cargo workspace** (WS-01) | Without `engineer-bin` and `wizard-bin` feature gates wiring two `[[bin]]` targets to shared crates, there is no compile-time MCP exclusion — which is the core value proposition (PROJECT.md line 9). Even if wizard is a hollow `fn main(){}` at M1, the workspace topology must exist. | M | FORK-01; STRIP-01 (dead crates removed first so workspace compiles cleanly) | M2 fills engineer-side logic; M3 fills wizard-side logic. Topology does not change. |
| **Dead crate removal** (STRIP-01) | `zeroclaw-hardware`, `robot-kit`, `aardvark-sys`, `apps/tauri`, `zeroclaw-plugins` are not used by sovereign-shield and represent ~30-40% of compile time + binary size + CVE surface. Workspace `Cargo.toml` `members = [...]` shrinks; transitive deps drop. | M | FORK-01 | None — these crates never come back. Quarterly upstream sync (FORK-02) must re-strip if upstream changes them. |
| **Channel strip to 6** (STRIP-02) | Keeping 26 unused channel implementations (Discord, IRC, XMPP, Teams, Lark, WeChat, etc.) is dead code carrying parser/auth/webhook surface. M1 keeps only Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal — the v1 customer set per decision #2. | M | WS-01 (need feature flag wiring); STRIP-01 | M2 adds Mattermost+Matrix runtime hookup; M4 adds WhatsApp+Signal runtime. Source-level strip done at M1 means M2/M4 are wiring jobs, not "first add the code back." |
| **Provider strip to 5** (STRIP-03) | Same logic: ~55 unused LLM providers carry API client + auth + retry code. Keep Anthropic, Gemini, Kimi-code (via openai-compatible base), Ollama, OpenRouter. Oracle (`ola-management-oracle`) maps onto Ollama-compatible at config layer, no separate provider. | M | STRIP-01 | None — provider set is intentionally narrow. New providers re-litigate decision #19/#20 policy modes. |
| **Tool strip** (STRIP-04) | ~35 tools removed (browser, web_search, hardware_*, jira, notion, microsoft365, voice_*, etc.). What survives at M1 is the minimal "engineer can answer in chat + run shell + read/write files" set; native bridge tool arrives in M2. | M | STRIP-01 | M2 adds `bridge` tool (native AMQP); M2 adds `exchange` channel-as-tool; M3 adds Vault-write tools on wizard side. |
| **Gateway strip with `/ws/chat` preservation** (STRIP-05) | OS-MDashboard's `chat-relay.ts` connects to zeroclaw's `/ws/chat` with bearer auth from `paired_tokens`. That code path is load-bearing for the existing dashboard and MUST keep working post-M1 cutover. Everything else in the gateway sub-surface (REST `/config`, `/onboarding`, `/pairing`, `/personality`, `/plugins`, `/webauthn`, ACP bridge, SSE, embedded web UI, mTLS server option, outbound webhooks) is dropped. | L | STRIP-01 | None — gateway shape is locked. M3 wizard reuses the same `/ws/chat` mechanism, doesn't add new HTTP. |
| **Webhook channel explicitly NOT included** (STRIP-06) | User rejected on security grounds (PROJECT.md line 34). M1 must not just "not enable" webhooks — it must remove the channel implementation entirely so a config flag can't re-introduce it. | S | STRIP-02 (handled in same pass) | Permanent. Adding webhooks back is a v2 decision requiring re-litigation. |
| **Telemetry audit + strip** (TELEMETRY-01) | A self-hosted security platform cannot ship binaries that phone home to third parties (Sentry, PostHog, Honeycomb, vendor analytics). Audit must enumerate every outbound non-LLM HTTP call and remove or gate it. Findings document goes in `sovereign-shield-backup/documentation/osAgent/TELEMETRY_AUDIT.md`. | M | None (can run in parallel with STRIP work) | Quarterly upstream sync (FORK-02) must re-audit because upstream may add new telemetry. |
| **MANIFEST.toml build artifact** (MANIFEST-01) | Per decision #24. Operator must be able to answer "what channels/providers/tools does this binary contain?" without reading source. Ships next to binary; `osagent manifest --diff config.toml` reports drift (the diff CLI itself is a differentiator below). | S | STRIP-02/03/04 (the manifest *describes* the strip result) | M2+ adds new tools → manifest auto-updates from build metadata; no manual maintenance. |
| **Drop-in install ansible task** (INSTALL-01) | `install_osagent.yml` must replace `install_zeroclaw.yml` cleanly: same systemd unit names (`engineer.service`), same paths (`/usr/local/bin/engineer`), same env file location, same ExecStartPre preflight. Per decision #31, binary naming is `engineer`/`wizard` (not `osagent-engineer`/`osagent-wizard`) at install time — the workspace binary names get renamed during install. M1 covers engineer only; wizard install stays on old zeroclaw until M3. | M | WS-01 (need a binary to install); plan-then-execute discipline (PROJECT.md constraint, line 133) | M3 adds wizard install task; M4 adds wizard cutover. |
| **License & attribution preservation in `NOTICE`** (FORK-01 subcomponent) | `LICENSE-APACHE`, `LICENSE-MIT`, upstream `NOTICE` retained verbatim; osAgent `NOTICE` adds our authorship line. Without this M1 is non-distributable. | S | FORK-01 | Reviewed on every quarterly upstream sync. |

### Differentiators (sets osAgent apart from a generic "strip zeroclaw" hack-job)

These are what make M1 produce a **sustainable** fork rather than a one-off branch that rots. A weekend-hack strip-down would deliver the table stakes but skip these — and would be unmaintainable within a quarter.

| Feature | Value Proposition | Complexity | Dependencies | M2+ Continuation |
|---------|------------------|------------|--------------|------------------|
| **MCP-exclusion CI gate** (WS-02) | `nm osagent-wizard \| grep -i mcp` as a CI-required check is the **load-bearing safety property** for the entire project. Without the gate, decision #1 (compile-time MCP exclusion) is just a code-review intention that drifts the first time someone adds a "small" MCP-adjacent dep to a shared crate. The gate is what makes the property *provable*. M1 ships it now even though wizard is hollow, so M3 cannot regress. | M (CI workflow + reliable symbol check across glibc/musl + clear failure message) | WS-01 (need a wizard binary to nm); GitHub Actions runner config (decision #40) | Permanent CI requirement. Failure on PR blocks merge. |
| **Manifest diff command** (`osagent manifest --diff <config.toml>`) | Per decision #24. Closes the gap between "what's compiled in" and "what's configured to be enabled." Operator runs it as part of `ExecStartPre`; mismatch (config references provider/channel not in manifest) = refuse-to-start. This is the runtime expression of the build-time manifest — without it the manifest is documentation, not enforcement. | M | MANIFEST-01 | M2+ adds enforcement to runtime config-load; M3 wizard ships its own variant. |
| **Quarterly upstream-sync runbook documented** (FORK-02) | Per decision #23. Without a written, tested runbook, "merge upstream quarterly" devolves into "merge upstream when somebody remembers." Runbook lives at `sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md` and includes: branch hygiene, conflict triage policy, re-strip checklist (the crates/channels/providers/tools that must stay stripped post-merge), `upstream-tag-N` CI integration suite, sign-off gate. | M | FORK-01 | Executed every quarter forever; runbook itself revised when upstream restructures (e.g., if zeroclaw moves to a different workspace layout). |
| **Drop-in install task structure** (INSTALL-01 done well) | The differentiator beyond "task exists" is that `install_osagent.yml` mirrors `install_zeroclaw.yml`'s task ordering, variable names, and the 14 invariant phase orderings documented in install-guide's CLAUDE.md. The sharp-cutover (decision #30) works only if the new task is structurally compatible. A naive "ship a new task with our preferred shape" breaks the cutover and fragments the install-guide's mental model. | M | INSTALL-01 baseline; familiarity with install-guide CLAUDE.md invariants | M3 wizard install task follows same structural template; M4 cutover is "swap variable, re-apply." |
| **Stripped Fluent i18n pipeline (en-only)** (decision #35) | Keep the Mozilla Fluent runtime, drop non-en locale files. Differentiator vs. naive strip: keeping the pipeline means M2+ can re-add locales (or customer-specific phrasing in customer-config) without restoring removed deps. Cheap to do at M1, expensive to undo if you ripped out Fluent entirely. | S | STRIP-01 (Fluent crate is in shared deps, not a strip target) | M2+ customer-config could override en strings; v2 could re-add locales. |
| **Version metadata: `0.1.0+zeroclaw-0.7.5`** (decision #33) | Independent semver with build-metadata suffix marking the upstream tag fork-point. Differentiator: every binary self-identifies its upstream lineage, which is critical for the quarterly sync runbook (which tag merged where) and for the audit trail when a CVE drops against upstream version X. | S | FORK-01 | Each upstream sync bumps the build-metadata suffix; our semver moves independently. |

### Anti-Features (NEVER in M1 — document the prohibition so future Claude sessions don't re-add)

These are features that would appear plausible to a future Claude session reading the M1 scope and "improving" it. The WHY is mandatory because the prohibition has to survive context loss.

| Feature | Why Tempting | Why Anti (PERMANENT prohibition) | Alternative |
|---------|--------------|----------------------------------|-------------|
| **Webhook ingress channel** | "Generic ingress is convenient — any weird customer stack can post to a webhook." Many agent frameworks ship this. | User explicitly rejected on security grounds (PROJECT.md line 34, decision-set constraint, line 129). A generic ingress is hard to harden against replay, signature spoofing, header injection, and rate-abuse. Each one we'd "make secure" is a CVE waiting. User preference is documented verbatim: *"rather build a custom osAgent update for a weird stack than overengineer and give the capabilities to get hacked."* | Per-customer custom channel adapter when a weird stack truly needs it. Treated as a fork-specific patch, not a generic capability. |
| **MCP on wizard binary** | "MCP servers are useful — surely we can sandbox them on wizard too." The MCP ecosystem is large and growing; engineers will be tempted to share infrastructure between binaries. | Decision #1 + line 9 of PROJECT.md: **wizard handles Vault writes; MCP servers are arbitrary-code-execution surfaces.** Compile-time exclusion is the load-bearing property. Adding MCP to wizard "with a sandbox" is exactly the config-drift hazard PROJECT.md line 64 calls out. CI gate `nm osagent-wizard \| grep -i mcp` enforces this — if you find yourself wanting to relax the gate, you are wrong. | MCP stays exclusively on engineer. If wizard needs an MCP-like capability, it goes through engineer via the operator AMQP bridge with the 45-verb allowlist. |
| **Multi-tenancy at config layer** | "Why not allow multiple customer_ids in one osAgent instance? Saves processes." Industry default is multi-tenant. | Decision-set constraint (PROJECT.md line 53, line 132): **single-tenant invariant**. Every path must start with `/opt/sovereign-shield/<customer_id>/`. Multi-tenancy at the config layer means cross-customer data leakage one bug away. The sovereign-shield platform model is **one customer = one deployment**; multi-tenancy is a fundamental architectural rejection. Decision #6 reinforces: Vault path prefix is per-customer-id and asserted at write time. | Run multiple osAgent processes (one per customer) on the same host or different hosts. Single-tenant invariant is asserted at config-load — refuse to start if violated. |
| **Sandbox auto-detect chain** (Auto→Landlock→Firejail→Docker→Noop) | "Robustness! Probe what's available, pick the best!" Upstream zeroclaw ships exactly this default. | PROJECT.md line 130: *"zeroclaw's default Auto→Landlock→Firejail→Docker→Noop probe is what bit us in 2026-04-22 (Docker wrap broke engineer's bridge access)."* Auto-detect is the kind of "helpful magic" that silently changes behavior across environments. osAgent ships **single `none` backend**; config-load rejects `sandbox.enabled != false` unless a build feature explicitly overrides. | Custom Landlock sandbox is a v2 candidate (PROJECT.md line 49) when we can ship one we control end-to-end. For M1: `sandbox.enabled=false` only. |
| **Silent failover in local-only provider policy** | "Cloud fallback when oracle is unreachable is more reliable!" Standard fault-tolerance pattern. | Decision #19 + PROJECT.md line 131: **airgap customers depend on the property that "local-only" means "no cloud traffic, period."** Silent failover is the worst kind of bug for a security platform — the customer's compliance posture changes without their knowledge. osAgent emits an alert and **refuses to serve** when oracle is unreachable in local-only mode. | Loud failure + alert. Customer chooses `local-first` if they want fallback; `local-only` is the hard-wall mode. |
| **Outbound webhook subscriptions** | Mirror of inbound webhooks — "let agents notify external systems via HTTP POST." | PROJECT.md line 51: same security posture as inbound webhook channel. Outbound HTTP-to-arbitrary-URLs is an exfiltration channel hard to constrain (DNS rebinding, internal-network targeting, etc.). | Per-customer custom adapter when needed. Default outbound is via the allowed channels (Telegram, Slack, etc.) only. |
| **Public artifact distribution (signed binaries on GitHub Releases)** | "Easy distribution! Customers can `wget` the binary!" Standard OSS practice. | PROJECT.md line 52: *"ship via our own infrastructure first."* Public artifact distribution requires release engineering (signing keys, supply-chain attestations, vulnerability comms) we don't have stood up. v2 candidate. | M1 ships via the install-guide ansible task pulling from our own artifact path. Customers don't `wget` anything. |
| **Coexist forever (old zeroclaw + osAgent both supported)** | "Migration safety! Customers choose when to switch!" Standard enterprise compatibility play. | Decision #30: **sharp cutover.** PROJECT.md line 55: *"Coexist doubles maintenance forever."* Existing installs migrate on next ansible apply; install-guide sharp-cutover bundle handles it. Coexist mode is a permanent tax that never gets paid down. | Sharp cutover via install-guide. The M1 install task is structured as a drop-in replacement, not a coexistence option. |
| **Subagent grand-children (depth > 1)** | "Compositional power! Subagents that spawn subagents!" Standard agent-framework feature. | Decision #14 + PROJECT.md line 50: **fork-bomb hazard.** Even with cost pooling (decision #15), depth>1 amplifies cost and audit-trail complexity unboundedly. Not an M1 concern (subagents are M3), but documented here so M3 doesn't backslide. | One-level deep enforced at runtime. Engineer can spawn subagent; subagent cannot. |
| **MCP server registry on engineer at M1** | "Just enable the MCP feature, it's already in upstream." | Not an M1 anti-feature for engineer (engineer **does** allow MCP per decision #1) — but enabling it at M1 is premature scope. M1 is "shape, not behavior." MCP-on-engineer is M2 territory when the native bridge tool replaces the shell-invoked bridge. | Defer to M2. Compile-flag wires it through, but the engineer's tool registry at M1 doesn't enable MCP until M2 confirms the bridge migration. |
| **REST endpoints for config/onboarding/pairing/personality/plugins/webauthn** | "Some customer might want HTTP API access!" Upstream ships these. | STRIP-05 explicitly drops them. They are unauthenticated-by-default or weakly-authenticated, and the wizard is the canonical writer-to-Vault path; HTTP config endpoints would compete with wizard's role and create config-drift risk. | `/ws/chat` (kept) is the only HTTP surface. OS-MDashboard talks to it with `paired_tokens` bearer auth. Anything else goes through chat or AMQP. |
| **Embedded web dashboard / pairing UI** | "Self-contained! No external dashboard dep!" Upstream ships an embedded React/HTMX UI. | STRIP-05 drops it. OS-MDashboard is the canonical UI surface (decision #3). Embedded UI duplicates that role, creates a second HTTP attack surface, and forces us to maintain JS toolchain inside the Rust binary. | OS-MDashboard via `/ws/chat`. No alternate UI. |
| **mTLS server option on the gateway** | "Defense in depth! mTLS for OS-MDashboard connection!" | STRIP-05 drops the mTLS *server* option. The dashboard auth path uses `paired_tokens` bearer auth (kept). mTLS server cert provisioning on the gateway would duplicate the install-guide's mTLS work (which is for AMQP), introducing certificate-management surface for negligible gain. | `paired_tokens` bearer auth on `/ws/chat`. mTLS lives on the AMQP path where it already works. |

---

## Feature Dependencies

```
FORK-01 (public fork + attribution)
    └──enables──> STRIP-01 (dead crate removal)
                       └──enables──> WS-01 (two-binary workspace)
                                          ├──enables──> WS-02 (MCP-exclusion CI gate)
                                          ├──enables──> STRIP-02 (channel strip — needs feature flags)
                                          ├──enables──> STRIP-03 (provider strip)
                                          ├──enables──> STRIP-04 (tool strip)
                                          └──enables──> STRIP-05 (gateway strip with /ws/chat kept)
                                                              └──enables──> MANIFEST-01 (manifest reflects strip)
                                                                                 └──enables──> manifest-diff command

FORK-01 ──enables──> FORK-02 (upstream-sync runbook)
                          (parallel with strip work; runbook
                           can be written before next sync hits)

TELEMETRY-01 (telemetry audit + strip)
    ──parallel with──> STRIP-01..05
    (independent pass — auditing outbound HTTP calls, not removing crates)

STRIP-06 (no webhook channel) ──merged into──> STRIP-02 (same pass)

WS-01 ──enables──> INSTALL-01 (need an engineer binary to install)
                        └──requires──> plan-then-execute discipline
                                         (install-guide CLAUDE.md hard rule)
```

### Dependency Notes

- **STRIP-01 (dead crates) before WS-01 (two-binary workspace):** Trying to wire the two `[[bin]]` targets while `apps/tauri` and the hardware/robot/aardvark crates are still in `members = [...]` means every workspace build still pulls their deps. Strip the crates first, then restructure.
- **WS-01 before WS-02:** The CI gate needs a wizard binary to `nm`. Even a hollow `fn main(){ std::process::exit(0); }` wizard is sufficient at M1 — the gate just needs symbols to inspect. Hollow wizard + working gate at M1 means M3 wizard development cannot regress the property.
- **WS-01 before STRIP-02/03/04:** Channel/provider/tool strips are done via Cargo features wired into the two-binary topology. Strip without the topology = source-level deletes that break upstream merges. Strip with the topology = feature flags off + dead-code removal, cleanly mergeable.
- **STRIP-02/03/04/05 before MANIFEST-01:** Manifest *describes* the strip outcome. Generating it before the strip is settled means regenerating it after every strip iteration.
- **MANIFEST-01 before manifest-diff CLI:** Diff command reads the manifest; no manifest, no diff.
- **TELEMETRY-01 in parallel:** Telemetry audit (grep for `reqwest`, `ureq`, `sentry`, `posthog`, `honeycomb`, env-driven URLs, etc.) doesn't touch the workspace structure. Can run alongside the strip work; findings feed into STRIP-04 (some tools may be removed entirely if they're purely telemetry-driven).
- **INSTALL-01 last:** Needs a buildable binary. Plan-then-execute discipline (per install-guide CLAUDE.md hard rule, PROJECT.md line 133) means **before any edit** to `sovereign-shield-install-guide`, emit a Touch/Change/Impact/Rollback plan. This is a process constraint, not a code dependency, but it gates INSTALL-01 work.
- **FORK-02 runbook is independent:** Can be drafted any time after FORK-01. Doesn't gate code work. Should land before M1 closes so M2's first upstream-tag dropping during M2 doesn't catch us flat-footed.

---

## MVP Definition (M1 = the MVP of the fork itself)

M1 IS the MVP — there is no "smaller M1." Every Active requirement (FORK-01 through INSTALL-01) is load-bearing for M2+. Cutting any of them creates a debt that M2 has to pay before adding behavior.

### Launch With (M1 ship list — all 12 requirements)

- [ ] **FORK-01** — Public fork + attribution. Without it, M1 is non-distributable.
- [ ] **FORK-02** — Upstream-sync runbook. Without it, the fork rots within a quarter.
- [ ] **WS-01** — Two-binary workspace. Without it, no compile-time MCP exclusion possible.
- [ ] **WS-02** — MCP-exclusion CI gate. Without it, the load-bearing safety property is unenforced.
- [ ] **STRIP-01** — Dead crate removal. Without it, the workspace doesn't build cleanly and binary size remains bloated.
- [ ] **STRIP-02** — Channel strip (keep 6). Without it, 26 unused channels carry CVE surface.
- [ ] **STRIP-03** — Provider strip (keep 5). Without it, 55 unused provider clients carry auth/HTTP surface.
- [ ] **STRIP-04** — Tool strip (~35 dropped). Without it, browser/voice/social tool surface persists.
- [ ] **STRIP-05** — Gateway strip with `/ws/chat` kept. Without it, REST/SSE/ACP/embedded-UI surfaces persist.
- [ ] **STRIP-06** — Webhook channel NOT included. Without explicit removal, a config flag could re-introduce it.
- [ ] **TELEMETRY-01** — Audit + strip phone-home. Without it, a self-hosted security platform ships phone-home binaries — disqualifying.
- [ ] **MANIFEST-01** — Build emits MANIFEST.toml. Without it, runtime config-vs-build drift is invisible.
- [ ] **INSTALL-01** — Ansible drop-in task (engineer only). Without it, the cutover (decision #30) has nowhere to land.

### Add After M1 (the M2/M3/M4 trajectory — included here for continuation context, not for M1 scope)

- **M2 (engineer-side behavior):** native bridge tool (replacing shell-invoked bridge), exchange channel-as-tool, lifecycle gates (CancellationToken into every tool per decision #21), sqlcipher memory backend (decision #9), audit dual-sink + hash-chain (decision #22), Mattermost+Matrix runtime, manifest-diff runtime enforcement at config-load.
- **M3 (wizard-side):** wizard binary fully fleshed out, 2-person approval (decision #5), bootstrap secret (decision #7), idempotency keys for Vault writes (decision #8), subagent primitive depth=1 (decisions #14-18), signed skill provenance (decision #27), wizard install task.
- **M4 (production):** WhatsApp+Signal runtime, 4-word codeword challenge (decision #10), oracle routing (decisions #19/#20), ops CLIs (`osagent rotate-channel-secret`, `osagent-rescue`, decisions #12, #26, #36), wizard cutover (the install-guide swap), full Arc42 docs in sovereign-shield-backup.

### Future Consideration (v2+)

- **Custom Landlock sandbox v1** — Not for M1 (`sandbox.enabled=false` only); v2 candidate when we can ship a sandbox we control end-to-end.
- **Teams channel** — Net-new Bot Framework + Azure AD work. v2.
- **APAC channels (Lark/Feishu, WeCom, DingTalk, WeChat, QQ, LINE)** — No customer demand. v2 if APAC GTM begins.
- **Public artifact distribution** — Release engineering work. v2.
- **Re-add non-en locales** — Pipeline preserved; locales themselves removed. v2 if customer demand.

---

## Feature Prioritization Matrix

| Feature | User Value | Implementation Cost | Priority | Notes |
|---------|-----------|---------------------|----------|-------|
| FORK-01 (public fork + attribution) | HIGH | LOW | **P1** | Precondition |
| WS-01 (two-binary workspace) | HIGH | MEDIUM | **P1** | Topology enables everything |
| WS-02 (MCP-exclusion CI gate) | HIGH | MEDIUM | **P1** | Load-bearing safety property |
| STRIP-01 (dead crates) | HIGH | MEDIUM | **P1** | Unblocks workspace cleanup |
| STRIP-02 (channels) | HIGH | MEDIUM | **P1** | Surface reduction |
| STRIP-03 (providers) | HIGH | MEDIUM | **P1** | Surface reduction |
| STRIP-04 (tools) | HIGH | MEDIUM | **P1** | Surface reduction |
| STRIP-05 (gateway) | HIGH | LARGE | **P1** | Trickiest strip; `/ws/chat` preservation is the load-bearing detail |
| STRIP-06 (no webhooks) | HIGH | LOW | **P1** | Done in same pass as STRIP-02 |
| TELEMETRY-01 (phone-home strip) | HIGH | MEDIUM | **P1** | Required for self-hosted platform |
| MANIFEST-01 (build artifact) | MEDIUM | LOW | **P1** | Cheap, enables manifest-diff |
| INSTALL-01 (ansible drop-in) | HIGH | MEDIUM | **P1** | The cutover landing zone |
| FORK-02 (upstream-sync runbook) | MEDIUM | LOW | **P1** | Prevents fork rot |
| Manifest-diff CLI command | MEDIUM | LOW | **P1** | Decision #24 |
| en-only Fluent strip | LOW | LOW | **P1** | Done as part of STRIP-01 sweep; decision #35 |
| Version metadata `0.1.0+zeroclaw-0.7.5` | LOW | LOW | **P1** | One line in Cargo.toml; decision #33 |

**Every M1 feature is P1.** There is no P2/P3 inside M1 — anything that's not table-stakes or differentiator is M2+. The matrix exists for documentation; there is no prioritization decision to make within M1.

---

## Competitor / Reference Analysis

This section is intentionally minimal — osAgent is a tailored fork of one specific upstream (zeroclaw v0.7.5), and the M1 fork strategy is dictated by 42 ratified decisions, not by market analysis.

| Aspect | Upstream zeroclaw v0.7.5 | osAgent M1 | Why we diverge |
|--------|--------------------------|------------|----------------|
| Binary count | 1 (general-purpose) | 2 (engineer, wizard with compile-time MCP exclusion) | Threat-model separation (decision #1) |
| Channel count | 30+ | 6 (Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal) | Surface reduction; only customer-needed channels |
| Provider count | 60+ | 5 (Anthropic, Gemini, Kimi-code, Ollama, OpenRouter) | Surface reduction; oracle maps to Ollama-compatible |
| Sandbox | Auto-detect chain (Landlock→Firejail→Docker→Noop) | `none` only (auto-detect explicitly removed) | 2026-04-22 incident (Docker wrap broke bridge) |
| Telemetry | Upstream phone-home (audit pending) | Stripped (TELEMETRY-01) | Self-hosted security platform invariant |
| MCP | Available globally | Engineer only; wizard compile-time excluded with CI gate | Vault-write safety property |
| Webhook channel | Included | NOT INCLUDED | User security preference (PROJECT.md line 34) |
| Gateway surface | REST + SSE + ACP + `/ws/chat` + embedded UI + mTLS server + webhooks | `/ws/chat` + `paired_tokens` only | OS-MDashboard is the canonical UI (decision #3) |
| Distribution | (varies) | Our own infrastructure via install-guide ansible | No public artifact distribution at M1 (v2) |
| Tenancy | (varies) | Single-tenant invariant asserted at config-load | Sovereign-shield platform model |

No general-purpose competitor analysis is appropriate at M1. osAgent is a single-purpose tool for one platform; comparing it to LangChain agents or AutoGen or Letta would be category-confused.

---

## Sources

- **Primary:** `d:/Repositories/osAgent/.planning/PROJECT.md` — 42 ratified architectural decisions, M1 Active requirements list, Out-of-Scope reasoning, project constraints. HIGH confidence — these are user-ratified, not researched.
- **Secondary:** Prior-session context relayed by orchestrator (M2/M3/M4 deferred feature mapping). HIGH confidence — sourced from ratified roadmap.
- **Tertiary:** `sovereign-shield-install-guide` CLAUDE.md invariants (referenced in PROJECT.md constraints, line 128-133). HIGH confidence — established invariants of an existing repo.
- **No external web sources used.** M1 scope is fully determined by ratified internal decisions; no ecosystem survey would add information. External research would be appropriate for M2 (native bridge implementation patterns, sqlcipher key derivation) or M3 (2-person approval UX patterns, idempotency-key conventions) but is out of scope for M1.

**Confidence assessment:** HIGH across the board. M1 is not an open-ended design problem — it is the execution of a defined set of strip-and-restructure requirements against a known upstream. The features above are derived deterministically from the PROJECT.md Active list, with categorization (table-stakes / differentiator / anti-feature) and dependency mapping added.

---

*Feature research for: osAgent M1 (Foundation) — tailored fork of zeroclaw v0.7.5*
*Researched: 2026-06-12*
