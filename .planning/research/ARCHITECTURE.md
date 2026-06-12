# Architecture Research

**Domain:** Forked Rust Cargo workspace producing two compile-time-separated binaries (engineer + wizard) from a shared crate set, with a load-bearing CI gate that wizard's binary is provably MCP-free.
**Researched:** 2026-06-12
**Confidence:** HIGH (workspace mechanics, MCP boundary, Cargo feature unification semantics verified against rust-lang/rfcs and cargo book); MEDIUM on exact upstream zeroclaw module names (inferred from prior-session context, to be verified by Phase 1 read-only inventory).

## Headline Finding

**The wizard-no-MCP property cannot be enforced by a Cargo feature flag on a shared `zeroclaw-tools` crate.** Cargo's feature unification (resolver = "2") will silently re-include MCP code when both binaries are built in one `cargo build --workspace` invocation, because the union of features across all members in one resolution is what gets compiled. The CI gate would pass on isolated builds and fail on workspace builds — or worse, pass everywhere and ship an MCP-tainted wizard because the union snuck the symbols back in.

**Therefore: MCP must live in a *separate crate* (`osagent-tools-mcp`) that the wizard binary's `Cargo.toml` **never** lists as a dependency — not even an optional one.** Feature flags are fine for variant *configuration*; structural exclusion requires *the dependency edge to not exist at all*.

This is decision #1 of the workspace restructure and drives the rest of the layout.

## Standard Architecture

### System Overview — osAgent in sovereign-shield

```
+----------------------------------------------------------------------------+
|                          OS-MDashboard (Next.js)                            |
|        chat-relay.ts  --bearer-token-->  WS /ws/chat (wizard binary)        |
+----------------------------------------------+-----------------------------+
                                               |
                                               v
+----------------------------------------------+-----------------------------+
|                     osAgent process tree (per customer)                     |
|                                                                             |
|   +----------------------+                +----------------------------+    |
|   |  osagent-engineer    |                |   osagent-wizard           |    |
|   |  (/usr/local/bin/    |                |   (/usr/local/bin/wizard)  |    |
|   |   engineer)          |                |                            |    |
|   |                      |                |   * NO MCP (CI-enforced)   |    |
|   |  + MCP tools         |                |   * Vault writes           |    |
|   |  + Native bridge tool|                |   * 2-of-2 approval        |    |
|   |  + 6 channels        |                |   * 6 channels             |    |
|   |  + WS /ws/chat       |                |   * WS /ws/chat            |    |
|   +----------+-----------+                +-------------+--------------+    |
|              |                                          |                   |
|              | shared crates (link both)                |                   |
|              +-------+---------+-------+----------------+                   |
|                      |         |       |                                    |
|                      v         v       v                                    |
|       +------------------+ +-------+ +------------------+                   |
|       | osagent-runtime  | |  ...  | | osagent-exchange |  (file-based     |
|       | osagent-config   | |       | |  PLAN/MISSION/   |   PLAN/MISSION/  |
|       | osagent-providers| |       | |  REPORT channel) |   REPORT bus     |
|       | osagent-memory   | |       | +--------+---------+   between        |
|       | osagent-channels | |       |          |             engineer +     |
|       | osagent-gateway- | |       |          |             wizard)        |
|       |   ws-only        | |       |          |                            |
|       | osagent-tools-   | |       |          v                            |
|       |   core           | |       |   /var/lib/sovereign-shield/          |
|       | osagent-tools-mcp| |  <----+   exchange/{plans,missions,reports}/  |
|       |   (eng only)     | |       |                                       |
|       | osagent-bridge   | |       |                                       |
|       | osagent-lifecycle| |       |                                       |
|       | osagent-audit    | |       |                                       |
|       | osagent-subagent | |       |                                       |
|       +------------------+ +-------+                                       |
+-----------------------------------------------------------------------------+
            |              |                |              |
            v              v                v              v
+-----------+--+ +---------+----+ +---------+-------+ +----+-------------+
| operator     | | witness      | | ola-management- | | Vault (mTLS,     |
| (AMQP, 45-   | | (hash-chain  | | oracle (Ollama  | |  customer-scoped |
| verb         | |  daily       | | proxy / cloud   | |  paths only)     |
| allowlist)   | |  anchor)     | | LLMs)           | |                  |
+--------------+ +--------------+ +-----------------+ +------------------+
            ^              ^                ^              ^
            |              |                |              |
            +-------+--------+-------+      |              |
                    | engineer       |      |              |
                    | only           |      |              |
                                            |              |
                              (both binaries call provider) |
                                                            |
                                              (wizard only — engineer
                                               writes Vault only via
                                               operator verb, not direct)
```

### Component Responsibilities

| Component | Responsibility | Implementation in osAgent |
|-----------|----------------|---------------------------|
| `osagent-config` | TOML/YAML schema, secrets loading, path-prefix invariant assertion (`/opt/sovereign-shield/<customer_id>/`), sandbox=`none` enforcement | Renamed `zeroclaw-config`; strip Landlock/Firejail/Docker auto-detect chain |
| `osagent-runtime` | Agent loop, observability (tracing/metrics, no phone-home), cron, SOP, skills, lifecycle pause-gates, `CancellationToken` propagation | Renamed `zeroclaw-runtime`; TUI extracted; hardware excised; telemetry-strip done here per TELEMETRY-01 |
| `osagent-providers` | LLM provider impls (Anthropic, Gemini, Kimi via openai-compatible, OpenRouter, Ollama/oracle); auth; multimodal | Trimmed ~50/60 providers; trait registry compile-time-pruned (see Pattern 2) |
| `osagent-memory` | sqlcipher (customer-derived key) + markdown only | Strip qdrant, postgres, embeddings, consolidation, community-skill HTTP |
| `osagent-channels` | 6 keepers (Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal) + dispatch trait + outbox | Source-level deletion of 24 channels; trait registry pruned (Pattern 2) |
| `osagent-gateway-ws-only` | ONLY `/ws/chat` + paired_tokens bearer auth; nothing else | New crate (or aggressively-feature-gated subset of `zeroclaw-gateway`). REST/SSE/ACP/dashboard/webhook code physically deleted, not just compiled out |
| `osagent-tools-core` | File, grep, glob, shell (allowed_commands), git, http_request (engineer-only), the ~25 tools we keep — **no MCP** | Forked from `zeroclaw-tools`; MCP subdirectory moved out |
| `osagent-tools-mcp` | mcp_client, mcp_protocol, mcp_transport, mcp_deferred | **NEW, isolated crate.** Wizard's `Cargo.toml` does not list this in any form. Engineer's does. |
| `osagent-bridge` | Native Rust AMQP+mTLS+operator-allowlist tool (replaces the shell→bash→python3→bridge chain) | NEW; lives behind the `engineer-bin` feature, depended on only by engineer-bin top crate |
| `osagent-exchange` | File-based PLAN/MISSION/REPORT channel under `/var/lib/sovereign-shield/exchange/`; both binaries read+write | NEW; shared crate, no MCP, no privileged surface |
| `osagent-lifecycle` | Pause-gate semantics, `CancellationToken` plumbing, Vault-write transactional completion | NEW; shared; integrates with audit for halt-event provenance |
| `osagent-audit` | Hash-chained append-only file + journald dual-sink; daily anchor format for witness | NEW; shared. Both binaries use it identically; sink path differs only by customer_id |
| `osagent-subagent` | Markdown frontmatter loader, depth=1 enforcement, pool-cost arithmetic, parent+sub identity in audit lines, signed provenance check | NEW; shared (engineer + wizard both spawn subagents; same code) |
| `osagent-api` | Trait definitions shared across crates | Renamed `zeroclaw-api`; minor pruning |
| `osagent-infra` | Channel session backends, debouncing, stall watchdog | Renamed `zeroclaw-infra`; channel-specific backends pruned |
| `osagent-tool-call-parser` / `osagent-macros` / `xtask` | Build tooling, codegen | Renamed; mostly unchanged |
| `osagent-engineer-bin` | Top-level binary crate. Depends on **all** shared crates + `osagent-tools-mcp` + `osagent-bridge`. `main.rs` only. | NEW top-level crate |
| `osagent-wizard-bin` | Top-level binary crate. Depends on **all** shared crates. Does **not** depend on `osagent-tools-mcp` or `osagent-bridge`. `main.rs` only. | NEW top-level crate |

## Proposed Workspace Layout

```
osAgent/
├── Cargo.toml                          # [workspace] manifest, resolver = "2"
├── rust-toolchain.toml                 # pin upstream's toolchain initially
├── MANIFEST.toml.in                    # build-time template, filled by xtask
├── .github/workflows/
│   ├── ci.yml                          # PR build matrix
│   ├── release.yml                     # signed release on self-hosted runner
│   └── upstream-sync.yml               # quarterly upstream-tag-N integration
├── bins/
│   ├── osagent-engineer/               # ELF: /usr/local/bin/engineer
│   │   ├── Cargo.toml                  # deps include osagent-tools-mcp + osagent-bridge
│   │   └── src/main.rs                 # ~50 lines: wire shared crates, start runtime
│   └── osagent-wizard/                 # ELF: /usr/local/bin/wizard
│       ├── Cargo.toml                  # NO osagent-tools-mcp, NO osagent-bridge
│       └── src/main.rs                 # ~50 lines: wire shared crates, start runtime
├── crates/
│   ├── osagent-api/                    # shared trait defs
│   ├── osagent-config/                 # schema + secrets + path invariants
│   ├── osagent-runtime/                # agent loop, observability, cron, SOP, skills
│   ├── osagent-providers/              # 5 LLM providers + oracle
│   ├── osagent-memory/                 # sqlcipher + markdown only
│   ├── osagent-channels/               # 6 chat channels + dispatch
│   ├── osagent-gateway-ws-only/        # /ws/chat + paired_tokens, nothing else
│   ├── osagent-tools-core/             # ~25 non-MCP tools
│   ├── osagent-tools-mcp/              # MCP client/protocol/transport/deferred
│   ├── osagent-bridge/                 # native AMQP+mTLS+operator-allowlist tool
│   ├── osagent-exchange/               # PLAN/MISSION/REPORT file bus
│   ├── osagent-lifecycle/              # pause-gate, CancellationToken propagation
│   ├── osagent-audit/                  # hash-chain dual-sink
│   ├── osagent-subagent/               # markdown-frontmatter + pool-cost + provenance
│   ├── osagent-infra/                  # session backends, debouncing, watchdog
│   ├── osagent-tool-call-parser/
│   └── osagent-macros/
└── xtask/
    └── src/main.rs                     # MANIFEST.toml emit, locale strip, nm check
```

### Cargo.toml workspace.members

```toml
[workspace]
resolver = "2"
members = [
  "bins/osagent-engineer",
  "bins/osagent-wizard",
  "crates/osagent-api",
  "crates/osagent-config",
  "crates/osagent-runtime",
  "crates/osagent-providers",
  "crates/osagent-memory",
  "crates/osagent-channels",
  "crates/osagent-gateway-ws-only",
  "crates/osagent-tools-core",
  "crates/osagent-tools-mcp",
  "crates/osagent-bridge",
  "crates/osagent-exchange",
  "crates/osagent-lifecycle",
  "crates/osagent-audit",
  "crates/osagent-subagent",
  "crates/osagent-infra",
  "crates/osagent-tool-call-parser",
  "crates/osagent-macros",
  "xtask",
]

# Shared dep versions — single source of truth, prevents drift.
[workspace.dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }
serde = { version = "1", features = ["derive"] }
# ...
```

### Structure Rationale

- **`bins/` vs `crates/` split:** binary crates are intentionally thin (~50 LOC `main.rs`) wrappers that only wire dependencies and call `runtime::run()`. All logic lives in `crates/`. This makes the MCP-boundary statement trivial to audit: *"open `bins/osagent-wizard/Cargo.toml` — if `osagent-tools-mcp` isn't listed, it cannot be linked."*
- **`osagent-tools-mcp` as a sibling crate, not a feature:** RFC 3692 and the resolver=2 docs are explicit that workspace builds unify features across selected members. If MCP were `osagent-tools = { features = ["mcp"] }` on engineer and `features = []` on wizard, then `cargo build --workspace` (which CI runs to verify everything compiles together) would unify and link MCP into both. Making it a separate dependency edge defeats unification entirely.
- **`osagent-gateway-ws-only` as a forked crate, not a stripped one:** Gateway upstream has REST + WS + ACP + dashboard + webhook + SSE + mTLS-server mixed in tightly. Forking the crate (rather than feature-gating the unwanted halves) ensures a security review can read `Cargo.toml` of this crate and see no rocket/axum REST router, no embedded static assets, no SSE handler — they are physically not in the source tree.
- **Top-level `bins/` instead of multi-bin in one crate:** Multi-`[[bin]]` in a single crate (i.e. `bins = [{name = "engineer", ...}, {name = "wizard", ...}]`) shares the crate's `[dependencies]` — both binaries would link MCP. Separate crates are mandatory.
- **`xtask` for codegen:** `MANIFEST.toml` is per-binary and reflects the actual compiled feature set; emitted by an xtask post-build that walks the binary's transitive dep graph. Same xtask runs the `nm`+`grep` check locally so developers can verify before pushing.

## MCP Boundary — How the Wizard CI Gate Is Structurally Guaranteed

### The Pattern

```
                                                  +------------------+
                                                  | osagent-tools-mcp|
                                                  |  (crate)         |
                                                  +--------+---------+
                                                           |
                                                           | depends on (engineer only)
                                                           |
+--------------------+                            +--------+---------+
| osagent-wizard-bin |                            | osagent-engineer-|
|   Cargo.toml       |                            |     bin          |
|                    |                            |   Cargo.toml     |
|  [dependencies]    |                            |                  |
|   osagent-runtime  | <----- shared crate ---->  |  [dependencies]  |
|   osagent-channels |        (same code,         |   osagent-runtime|
|   osagent-tools-   |         linked twice)      |   osagent-channels|
|     core           |                            |   osagent-tools- |
|   osagent-audit    |                            |     core         |
|   ...              |                            |   osagent-tools- |
|                    |                            |     mcp <-- HERE |
|  NO osagent-tools- |                            |   osagent-bridge |
|     mcp            |                            |   osagent-audit  |
|  NO osagent-bridge |                            |   ...            |
+--------------------+                            +------------------+
```

The `osagent-runtime` crate must not have `osagent-tools-mcp` as a dependency either — not even optional. If it did, building `osagent-runtime` standalone would pull MCP in transitively. Instead, `runtime` accepts a `dyn ToolRegistry` trait object at startup and the binary crate provides it (with or without MCP registered).

### Why feature flags don't suffice

```
# BROKEN approach — DO NOT use this pattern:
[package]
name = "osagent-tools"

[features]
default = []
mcp = []                       # gates the mcp_* modules

# In bins/osagent-engineer/Cargo.toml:
osagent-tools = { workspace = true, features = ["mcp"] }

# In bins/osagent-wizard/Cargo.toml:
osagent-tools = { workspace = true }  # no mcp feature
```

This *looks* correct. It is not. Run `cargo build --workspace` (which every dev runs, and which CI runs to verify all members compile) and Cargo's resolver=2 unifies features across all selected packages — `osagent-tools` is built **once** with `["mcp"]` because engineer enabled it, and **wizard links that same .rlib**. `nm wizard | grep mcp` passes a non-empty result. Property silently violated.

References: [RFC 3692](https://rust-lang.github.io/rfcs/3692-feature-unification.html), [Cargo Features docs](https://doc.rust-lang.org/cargo/reference/features.html), [Cargo Workspace and the Feature Unification Pitfall](https://nickb.dev/blog/cargo-workspace-and-the-feature-unification-pitfall/).

### Trait registry — Pattern 2

Channels, providers, and tools register via static trait objects today (upstream zeroclaw). To gate at the crate boundary we cannot use a `static REGISTRY: Lazy<Vec<Box<dyn Channel>>>` populated by `inventory::submit!` from each impl crate, because that pattern depends on all impl crates being linked. Instead:

```rust
// In osagent-api:
pub trait ChannelFactory: Send + Sync {
    fn name(&self) -> &str;
    fn build(&self, cfg: &ChannelCfg) -> Box<dyn Channel>;
}

// In each binary's main.rs:
let mut registry = ChannelRegistry::new();
registry.register(Box::new(osagent_channels::telegram::Factory));
registry.register(Box::new(osagent_channels::slack::Factory));
// ... 6 explicit lines, no inventory!, no auto-discovery.
```

Same pattern for tools and providers. The binary crate's `main.rs` is the explicit, auditable manifest of what is compiled in. Removing a `registry.register(...)` line removes the impl from the binary (and the dead-code linker eliminates the impl entirely if no other reference holds it). This is also what feeds `MANIFEST.toml` emission at build time.

## Build Matrix for CI

### `.github/workflows/ci.yml` (PR build)

```
jobs:
  build-engineer:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo build --release -p osagent-engineer-bin
      - run: |
          test -s target/release/engineer
          # informational, not gating
          nm target/release/engineer | grep -ci mcp || true

  build-wizard:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo build --release -p osagent-wizard-bin
      - name: WIZARD-NO-MCP gate (DECISION #25)
        run: |
          test -s target/release/wizard
          if nm target/release/wizard | grep -i mcp ; then
            echo "FAIL: wizard binary contains MCP symbols"
            exit 1
          fi
          echo "PASS: wizard binary is MCP-free"

  build-workspace:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # Verify the entire workspace compiles together (engineer + wizard +
      # shared crates) without the unification pitfall taking us by surprise.
      - run: cargo build --workspace
      - name: WIZARD-NO-MCP gate ALSO on workspace build
        run: |
          if nm target/debug/wizard | grep -i mcp ; then
            echo "FAIL: workspace build leaked MCP into wizard — feature unification regression"
            exit 1
          fi

  test-engineer:
    runs-on: ubuntu-latest
    steps: [ checkout, "cargo test -p osagent-engineer-bin" ]

  test-wizard:
    runs-on: ubuntu-latest
    steps: [ checkout, "cargo test -p osagent-wizard-bin" ]

  lint:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings

  manifest-diff:
    runs-on: ubuntu-latest
    steps:
      - run: cargo run -p xtask -- emit-manifest --bin engineer
      - run: cargo run -p xtask -- emit-manifest --bin wizard
      - run: cargo run -p xtask -- diff-manifest --against fixtures/expected/
```

### `.github/workflows/release.yml` (self-hosted, signed)

Same gates as PR plus:
- runs on `self-hosted` runner (DECISION #40)
- `cosign sign-blob` on each binary
- `cargo run -p xtask -- emit-manifest` and ship `MANIFEST.toml` alongside

### `.github/workflows/upstream-sync.yml` (quarterly)

- triggered manually after a `git fetch upstream && git merge upstream/main` PR is opened
- runs the **upstream-tag-N integration test suite** (DECISION #23): full build matrix + boot smoke tests for both binaries against a docker-composed sovereign-shield stub

## Data Flow

### Engineer request flow (Telegram → operator)

```
[ops user types !restart vault in Telegram group]
    ↓
[Telegram channel impl in osagent-channels] (engineer binary)
    ↓
[osagent-runtime agent loop] (engineer binary)
    ↓
[LLM provider — anthropic.tool_use returns: bridge.invoke({verb: "vault.restart"})]
    ↓
[osagent-bridge native AMQP tool] → AMQP `osagent.engineer.requests` queue
    ↓                                    ↓
[osagent-audit hash-chain line]    [operator service]
                                         ↓
                              [allowlist.json lookup — verb permitted?]
                                         ↓ yes
                              [systemctl restart vault.service]
                                         ↓
                              AMQP `osagent.engineer.responses` queue
                                         ↓
[osagent-bridge response]
    ↓
[runtime emits chat reply via Telegram]
    ↓
[osagent-audit hash-chain line]
```

### Wizard secret-write flow (dashboard → Vault)

```
[admin types "Rotate the postgres bootstrap password" in OS-MDashboard chat]
    ↓
[chat-relay.ts → POST /ws/chat with bearer token]
    ↓
[osagent-gateway-ws-only — paired_token auth] (wizard binary)
    ↓
[osagent-runtime] (wizard binary)
    ↓
[LLM tool_use: vault_write({path: "secret/data/customer42/postgres/bootstrap", value: ...})]
    ↓
[osagent-tools-core vault_write tool — DOES NOT use MCP]
    ↓
[path-prefix check: starts with "secret/data/customer42/"? YES]
    ↓
[idempotency-key check: hash(tool+args+correlation_id) seen before? NO]
    ↓
[osagent-lifecycle: emit pending-approval; CancellationToken still alive]
    ↓
[wait for 2-of-2 acks: dashboard ack + chat ack from distinct identities]
    ↓ (got both within 1h timeout)
[Vault HTTP write via direct mTLS — wizard reaches Vault directly, not via operator]
    ↓
[osagent-audit hash-chain line with parent+subagent identity if applicable]
```

### Engineer ↔ Wizard via osagent-exchange (file bus)

```
[wizard plans a multi-step install]
    ↓
[osagent-exchange::Plan::write(plan)]
    ↓
/var/lib/sovereign-shield/exchange/plans/2026-06-12T1043-customer42-postgres.plan.toml
    ↓
[engineer polls plans/ on cron tick — osagent-runtime cron module]
    ↓
[engineer reads plan, creates Mission record in missions/]
    ↓
[engineer executes mission → bridge → operator → ansible play]
    ↓
[engineer writes Report to reports/customer42-postgres.report.toml]
    ↓
[wizard reads report on next cron tick]
```

Both binaries link `osagent-exchange` (same code), only the filesystem ACL determines write paths.

### Audit → Witness daily anchor

```
[osagent-audit appends hash-chained line to /var/log/sovereign-shield/osagent-customer42.audit]
   (also writes to journald via tracing-journald)
    ↓
[osagent-runtime cron @ 00:00 UTC]
    ↓
[xtask-emitted anchor: take last hash of yesterday's audit file, post to witness AMQP queue]
    ↓
[witness service appends to its own hash-chain]
    ↓
[cross-link recorded: yesterday's osagent tip == witness chain entry N]
```

### Provider call (both binaries identical)

```
[runtime] → [provider trait obj from registry] →
   if policy == local-only:  [ola-management-oracle (Ollama proxy)]
                                ↓ if oracle down: REFUSE (no fallback)
   if policy == local-first:  try oracle → fallback to cloud on error
   if policy == cloud-first:  try Anthropic/Gemini/Kimi/OpenRouter → fallback to oracle
```

## Architectural Patterns

### Pattern 1: Explicit-Registration Trait Registry (No Auto-Discovery)

**What:** Each binary's `main.rs` lists every channel, provider, and tool by name in registration calls. No `inventory::submit!`, no `linkme::distributed_slice`, no auto-discovery.

**When to use:** When the compile-time set of impls is a security boundary, not a convenience.

**Trade-offs:** More boilerplate in `main.rs`; loses the ergonomic "drop a file in the channels dir and it's picked up." Gains: auditable in one file, no surprises from feature unification, dead-code-eliminator removes unregistered impls cleanly.

**Example:**

```rust
// bins/osagent-wizard/src/main.rs
fn build_channel_registry() -> ChannelRegistry {
    let mut r = ChannelRegistry::new();
    r.register(Box::new(osagent_channels::telegram::Factory::wizard()));
    r.register(Box::new(osagent_channels::slack::Factory::wizard()));
    r.register(Box::new(osagent_channels::mattermost::Factory));
    r.register(Box::new(osagent_channels::matrix::Factory));
    r.register(Box::new(osagent_channels::whatsapp_cloud::Factory));
    r.register(Box::new(osagent_channels::signal::Factory));
    r
}
```

### Pattern 2: Crate-Boundary Capability Exclusion

**What:** A capability that must be provably absent from a binary lives in its own crate, and that binary's `Cargo.toml` does not list the crate. The crate is not optional, not feature-gated, not behind a `cfg` — it is simply not a dependency.

**When to use:** Any compile-time safety property enforced by build tooling (CI gate, `nm` check, supply-chain attestation).

**Trade-offs:** More crates to manage; some code duplication if two crates share a small helper (extract to `osagent-api` or a small util crate). Gains: defeat-proof against feature unification, trivial to audit.

**Example:** `osagent-tools-mcp` is a separate crate. `bins/osagent-wizard/Cargo.toml` does not include it. Done.

### Pattern 3: Top-Level Binary Crate as Manifest

**What:** Binary crates are ~50 LOC `main.rs` that wires shared crates and starts the runtime. The binary crate's `Cargo.toml` is the explicit, human-readable manifest of what is compiled into that binary.

**When to use:** Two-or-more-binary projects where each binary has a different intended capability set.

**Trade-offs:** Boilerplate duplication between the two `main.rs` files (~80% identical). Worth it: every diff to the binary's `Cargo.toml` is visible in code review as a security-relevant change.

**Example:** See `bins/osagent-engineer/Cargo.toml` vs `bins/osagent-wizard/Cargo.toml` diff in the workspace layout above — the only differences are `osagent-tools-mcp` and `osagent-bridge` lines.

### Pattern 4: Source-Level Strip (Not Feature-Gate) for High-Risk Removals

**What:** When dropping ACP, REST endpoints, plugins, webhooks, or Microsoft Teams, *delete the source files* — don't `#[cfg(feature = "...")]` them out. Reasoning: a `cfg`-gated block is one toolchain bug or one accidentally-enabled feature away from being live again. Deleted code cannot accidentally compile.

**When to use:** Any capability the user has explicitly said is "rather build custom than overengineer the surface."

**Trade-offs:** Loses the ability to easily re-enable. Gains: smaller diff against `cargo-audit`, smaller `MANIFEST.toml`, easier security review.

**Example:** STRIP-05 (REST endpoints, ACP bridge, SSE, dashboard, webhook) deletes the source files in the forked `osagent-gateway-ws-only` crate. Re-enabling Teams in v2 means writing the integration fresh (or upstream-merging the file again), not flipping a flag.

### Pattern 5: Quarterly Upstream Merge via Subtree Replay

**What:** Track upstream zeroclaw as a remote. Quarterly, open a dedicated PR that merges `upstream/main` into `osagent-main`, run the upstream-tag-N integration test suite, manually resolve conflicts (which will be substantial because we renamed crates).

**When to use:** Forks of actively-developed upstream projects where security fixes matter but our restructure makes auto-merge impossible.

**Trade-offs:** Quarterly merge labor (estimated 2-4 hours per merge if upstream churn is moderate). Gains: free security fixes, free new providers (cherry-pickable), no drift from upstream's debugging community.

**Implementation:**
```bash
# documented in sovereign-shield-backup/documentation/osAgent/UPSTREAM_SYNC.md
git remote add upstream https://github.com/<upstream-owner>/zeroclaw
git fetch upstream
git checkout -b sync/upstream-2026-Q3 osagent-main
git merge upstream/v0.9.0       # conflicts expected on crate renames
# resolve, run xtask test suite, open PR with conflict resolution log
```

## Scaling Considerations

| Scale | Architecture Adjustments |
|-------|--------------------------|
| 1 customer (now) | Single workspace, single deploy. No changes needed. |
| 10 customers | Same binaries, deployed per-customer. `customer_id` in config + path prefix invariant prevents cross-talk. Build artifacts shared (sign once, deploy N). |
| 100 customers | Binaries unchanged. Centralized build pipeline emits signed artifacts; ansible pulls signed binary + per-customer config. xtask `MANIFEST.toml diff` becomes critical for fleet inventory. |
| Per-customer divergence | Customer-specific tools land in `osagent-tools-core` behind cfg(feature) **only if benign** (e.g. a customer's analytics SDK). Anything touching secrets — own crate, own `Cargo.toml` edit, own audit. |

### Scaling Priorities

1. **First bottleneck — build time:** ~24-crate workspace + heavy serde/tokio rebuilds. Mitigate with `sccache` (already standard) and the `bins/` split (developers iterating on engineer don't rebuild wizard).
2. **Second bottleneck — quarterly upstream merge labor:** if zeroclaw churns fast, conflicts grow. Mitigate by keeping our renames mechanical (`zeroclaw-X` → `osagent-X`) so a `sed` script can pre-process incoming upstream diffs.
3. **Third bottleneck — `MANIFEST.toml` drift between binaries and config:** customer config says "use channel X" but binary doesn't include it. Mitigate with `osagent manifest --diff config.toml` at boot (DECISION #24) — refuse to start on mismatch.

## Anti-Patterns

### Anti-Pattern 1: Feature-Gate MCP

**What people do:** Add `mcp` as an optional feature on `osagent-tools` (or `zeroclaw-tools`), enable it on engineer binary, leave it off on wizard.

**Why it's wrong:** Feature unification at workspace-build time (resolver=2 default behavior) silently re-includes MCP into wizard whenever `cargo build --workspace` runs. The CI gate using `nm` would catch this *only if CI builds each binary in isolation*; the workspace-build case (developers, integration tests, dependency-update PRs) ships MCP-tainted wizard to test environments and risks shipping it to production if any release pipeline ever changes to a workspace build.

**Do this instead:** Separate crate (`osagent-tools-mcp`), no dependency edge from wizard or from any crate wizard depends on. Pattern 2.

### Anti-Pattern 2: Multi-`[[bin]]` Crate

**What people do:** One crate (`osagent`) with `[[bin]] name = "engineer"` and `[[bin]] name = "wizard"` to share dependencies.

**Why it's wrong:** Both bins share `[dependencies]`. Anything one needs, both link. MCP-on-wizard inevitable.

**Do this instead:** Separate top-level crates under `bins/`, each with its own `Cargo.toml`. Pattern 3.

### Anti-Pattern 3: Auto-Discovery Registries (inventory, linkme, ctor)

**What people do:** Use `inventory::submit!` or `linkme::distributed_slice` so adding a channel/provider/tool is "drop a file in the directory, it's automatically registered."

**Why it's wrong:** Such macros work by emitting `#[link_section]` data that the linker collects. If a channel crate is linked (because a feature flag or transitive dep pulled it in), its `inventory::submit!` fires — even if no other code references it. This makes "what is compiled into this binary?" impossible to answer by reading `main.rs`; you have to read every dep.

**Do this instead:** Explicit `registry.register(Box::new(impl::Factory))` calls in `main.rs`. Pattern 1.

### Anti-Pattern 4: Cfg-Gate Capabilities That Should Be Source-Deleted

**What people do:** `#[cfg(feature = "webhook-channel")] mod webhook;` in `osagent-channels` for a capability we've explicitly rejected.

**Why it's wrong:** The webhook source is still in the tree. A future contributor enables the feature for "testing" and forgets. A `cargo build --all-features` (run by docs.rs, by clippy, by some CI checks) compiles it and links it. The decision becomes a runtime config rather than a source-tree property.

**Do this instead:** `rm -rf crates/osagent-channels/src/webhook/`. The decision is now permanent and version-controlled. Pattern 4.

### Anti-Pattern 5: Sandbox Auto-Detect Chain

**What people do:** Keep zeroclaw's `Auto → Landlock → Firejail → Docker → Noop` sandbox probe to "be safe by default."

**Why it's wrong:** Already burned us 2026-04-22 — Docker auto-wrap broke engineer's bridge access. The auto-probe makes behavior depend on which sandboxer happens to be installed on the host, which varies between dev/staging/prod. The same `osagent` binary behaves differently per host.

**Do this instead:** osAgent ships a single `none` sandbox backend. Config-load asserts `sandbox.enabled == false` (unless a build feature explicitly enables an alternative for a future hardened deployment). Constraint already documented in PROJECT.md.

## Integration Points

### External Services (sibling services in sovereign-shield)

| Service | Direction | Pattern | Notes |
|---------|-----------|---------|-------|
| operator (45-verb allowlist) | engineer → operator | AMQP via `osagent-bridge` (native Rust, mTLS) | Replaces today's shell→bash→python3→bridge chain. Allowlist read from `/etc/zeroclaw/operator/allowlist.json` (path keeps zeroclaw name for backward compat). |
| witness | both → witness (write); witness → audit files (read) | AMQP for daily anchor (DECISION #39); witness scrapes audit files directly | Anchor format: `{tip_hash, file_path, customer_id, date, sig}`. Cross-link recorded in witness's chain. |
| ola-management-oracle (Ollama proxy) | both → oracle | HTTP, OpenAI-compatible API | Provider crate's `oracle` impl. `local-only` policy refuses cloud fallback; `local-first` falls back. |
| Vault (mTLS) | wizard → Vault (write); engineer → Vault (read via operator only) | HTTPS + mTLS | Wizard reaches Vault directly. Engineer never writes Vault; reads only through an operator verb. Path prefix `secret/data/<customer_id>/` enforced in `osagent-tools-core::vault_write` before HTTP call. |
| OS-MDashboard chat-relay | dashboard → wizard | WS bearer token (paired_tokens) | The reason `osagent-gateway-ws-only` keeps `/ws/chat`. Engineer also exposes the same endpoint for symmetric dashboard chat. |
| 6 chat channels (Telegram, Slack, Mattermost, Matrix, WhatsApp-Cloud, Signal) | bidirectional | Each channel's native API | Bot tokens in Vault, rotated via `osagent rotate-channel-secret`. Outbox SQLite for replay (DECISION #13). |
| Anthropic / Gemini / Kimi / OpenRouter | both → provider | HTTPS | Optional per `cloud-first`/`local-first` policy. Disabled in `local-only`. |
| Telegram bot service (per-customer 2-bot pair) | bidirectional | Telegram Bot API | DECISION #41: engineer + wizard each own a distinct bot per customer. Identity isolation in chat. |

### Internal Boundaries

| Boundary | Communication | Notes |
|----------|---------------|-------|
| engineer ↔ wizard | filesystem via `osagent-exchange` PLAN/MISSION/REPORT | Both binaries link the same `osagent-exchange` crate. Coordination happens on disk under `/var/lib/sovereign-shield/exchange/`. No direct IPC, no shared memory, no socket. |
| binary crate ↔ shared crate | direct Cargo dep | Engineer's binary lists all shared crates + MCP + bridge. Wizard's binary lists all shared crates, NOT MCP, NOT bridge. |
| `osagent-runtime` ↔ tool registry | `dyn ToolRegistry` trait object passed at startup | Runtime doesn't know what tools are registered. Binary crate constructs the registry. This is the seam that makes the MCP boundary work. |
| `osagent-channels` ↔ channel impls | trait `ChannelFactory` in `osagent-api` | Each channel impl in its own submodule. Explicitly registered in `main.rs` (Pattern 1). |
| `osagent-audit` ↔ witness | hash-chained file + AMQP anchor | Witness reads files directly (scraper pattern), but daily anchor goes via AMQP for tamper-detection (DECISION #22, #39). |

## Build Order Across M1's 6 Phases

The roadmap will likely structure M1 as 6 phases. The crate restructure has internal dependencies — some strips can't happen until other restructures are in place. Recommended order:

### Phase 1 — Fork + read-only inventory (no code changes)
- **FORK-01, FORK-02:** Set up `andreas2301/osAgent`, `osagent-main` branch, attribution files, quarterly-sync runbook.
- **Read-only audit:** confirm exact upstream module locations for MCP (`zeroclaw-tools/src/mcp_*`), the channel registration pattern (is it `inventory::submit!` or explicit?), gateway's REST vs WS module split, telemetry code paths (TELEMETRY-01 audit only, no strip yet).
- **No code changes.** Output: a written inventory of what we found vs what the prior session assumed.

### Phase 2 — Workspace skeleton + binary split (WS-01)
- Rename `zeroclaw-*` → `osagent-*` (mechanical sed pass; keep `Cargo.toml` package names consistent with directory names).
- Create `bins/osagent-engineer/` and `bins/osagent-wizard/` top-level crates with ~50 LOC `main.rs` each. Both link the same crates initially (including MCP and bridge stubs on both).
- Migrate to Pattern 1 (explicit registration) for channels + providers + tools where upstream used inventory/linkme. This is the prerequisite for any later strip.
- Both binaries build green. Workspace builds green.

### Phase 3 — MCP boundary + wizard CI gate (WS-02, decision #25)
- Extract `crates/osagent-tools-mcp/` from `osagent-tools-core/`. Move `mcp_client`, `mcp_protocol`, `mcp_transport`, `mcp_deferred` modules.
- Edit `bins/osagent-wizard/Cargo.toml` to remove the `osagent-tools-mcp` dep. Remove the wizard's `registry.register(McpFactory)` lines.
- Add `.github/workflows/ci.yml` with the `nm ... | grep -i mcp` gate on both isolated and workspace builds.
- **This is the milestone-defining green check.** Until this passes, M1 is unfinished.

### Phase 4 — Whole-crate drops (STRIP-01, STRIP-06)
- Delete `zeroclaw-hardware/`, `robot-kit/`, `aardvark-sys/`, `apps/tauri/`, `zeroclaw-plugins/` from workspace members.
- Strip telemetry phone-home (TELEMETRY-01: full strip, not just audit).
- Delete webhook channel source (STRIP-06).
- Workspace builds green; binaries shrink measurably; `MANIFEST.toml` reflects.

### Phase 5 — Channel/provider/tool source-strips (STRIP-02, STRIP-03, STRIP-04)
- Now that Pattern 1 (explicit registration) is in place (from Phase 2), removing channel impls is just `rm -rf crates/osagent-channels/src/<channel>/` plus deleting the `registry.register(...)` line in both `main.rs` files.
- Same for providers (`crates/osagent-providers/src/<provider>/`) and tools (`crates/osagent-tools-core/src/<tool>/`).
- Memory-backend strip (qdrant, postgres, embeddings, consolidation, community-skill HTTP) lives in `osagent-memory/`.
- This phase is the bulk of LOC deletion (~60K lines). All deletions, no new code.

### Phase 6 — Gateway fork + install ansible (STRIP-05, MANIFEST-01, INSTALL-01)
- Create `crates/osagent-gateway-ws-only/` by copying from `zeroclaw-gateway` and deleting REST endpoints, SSE, ACP bridge, dashboard, webhook, mTLS-server, pairing dashboard. Keep `/ws/chat` and paired_tokens.
- Remove `zeroclaw-gateway` from workspace members.
- Implement `xtask emit-manifest` → `MANIFEST.toml` shipped with each binary (MANIFEST-01).
- Write `sovereign-shield-install-guide/ansible/install_osagent.yml` engineer-side only (wizard stays on old zeroclaw per INSTALL-01) — respect the 14 invariant phase orderings from install-guide CLAUDE.md, plan-then-execute, mTLS provisioning patterns.
- M1 ships.

### Why this order

- **Phase 2 (explicit registration) before Phase 5 (source strips):** can't safely delete a channel impl while `inventory::submit!` auto-discovery is still in play, because partial removal leaves dangling references. Pattern 1 first, deletions second.
- **Phase 3 (MCP boundary + CI gate) before Phase 4/5/6 (strips):** the gate is the load-bearing safety property. It must work — and be CI-enforced — before any other strip happens, so subsequent strips don't accidentally regress it. Also gives the team a concrete green-check milestone early.
- **Phase 4 (whole-crate drops) before Phase 5 (file-level strips):** dropping 5 entire crates is mechanically simple and shrinks the surface fast. Doing it before the harder file-level strips lets Phase 5 work on a smaller codebase.
- **Phase 6 (gateway fork + install ansible) last:** install ansible touches `sovereign-shield-install-guide`, which has plan-then-execute discipline and 14 invariant phase orderings. Ship it when everything else is stable so the install plan reflects the final binary shape, not an intermediate state.

## Confidence Notes

- **HIGH** on the feature-unification pitfall and the separate-crate solution: directly documented in RFC 3692 and the cargo book; multiple independent sources confirm.
- **HIGH** on Pattern 1 (explicit registration) being necessary to drop impls cleanly: this is standard Rust practice for capability-bounded binaries.
- **HIGH** on the data-flow direction for all 8 sibling services: derived from the 42 ratified decisions in PROJECT.md.
- **MEDIUM** on the exact upstream module names (`mcp_client`, `mcp_protocol`, etc.): inferred from prior-session context, will be verified during Phase 1 read-only inventory.
- **MEDIUM** on whether upstream zeroclaw currently uses `inventory::submit!` vs explicit registration for channels/tools — Phase 1 will confirm. If upstream is already explicit, Phase 2 becomes cheaper; if it's auto-discovery, Phase 2 is the most labor-intensive phase of M1.
- **LOW** on exact LOC counts for the strips (~60K lines is an estimate from prior-session context).

## Sources

- [RFC 3692: Feature Unification](https://rust-lang.github.io/rfcs/3692-feature-unification.html) — definitive: workspace-level feature unification semantics.
- [Cargo Book: Features](https://doc.rust-lang.org/cargo/reference/features.html) — `resolver = "2"` behavior, per-package feature syntax.
- [Cargo Book: Workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) — workspace.members, shared dependencies.
- [Cargo Book: Dependency Resolution](https://doc.rust-lang.org/cargo/reference/resolver.html) — unification rules.
- [RFC 2957: cargo-features2](https://rust-lang.github.io/rfcs/2957-cargo-features2.html) — resolver=2 design.
- [Cargo Workspace and the Feature Unification Pitfall — nickb.dev](https://nickb.dev/blog/cargo-workspace-and-the-feature-unification-pitfall/) — practical exposition of the pitfall this doc's headline finding is built on.
- [cargo-dist Issue #1740: Workspace feature resolution for multiple binaries](https://github.com/axodotdev/cargo-dist/issues/1740) — real-world report of the same pitfall biting a CI build matrix.
- [cargo Issue #4463: Feature selection in workspace depends on packages compiled](https://github.com/rust-lang/cargo/issues/4463) — upstream tracking of the unification surprise.
- [d:/Repositories/osAgent/.planning/PROJECT.md](file:///d:/Repositories/osAgent/.planning/PROJECT.md) — 42 ratified decisions, constraints, sovereign-shield integration map.

---
*Architecture research for: osAgent (forked Rust workspace, dual-binary, MCP-boundary-enforced)*
*Researched: 2026-06-12*
