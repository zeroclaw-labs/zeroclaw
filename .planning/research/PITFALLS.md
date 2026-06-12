# Pitfalls Research

**Domain:** Tailored Rust-workspace fork (zeroclaw v0.7.5 → osAgent) producing two compile-time-separated binaries, dropping into an ansible-deployed systemd environment with 14 documented prior failure-mode classes from `sovereign-shield-install-guide`.
**Researched:** 2026-06-12
**Confidence:** HIGH (Cargo / fork / install-guide invariants); MEDIUM (Rust trait-object monomorphization + dead-code edges); HIGH (the 11 prior-session incident classes — these are LIVED experience).

---

## Top-of-document: Answers to the five required questions

1. **Most common way Cargo feature gates leak** — workspace feature **unification** under resolver v1, and the **implicit `default-features = true` re-enable** path on transitive dependencies. If crate A depends on `tokio` with `default-features = false` but crate B in the same workspace depends on `tokio` (defaults left on), Cargo builds **one** copy of `tokio` with the **union** — A silently gets every default feature back. The wizard binary, even with all `#[cfg(feature = "mcp")]` blocks gated out at the wizard crate, can still pull in the MCP code if any shared crate has `mcp` in its default feature set or if `[features] default = [...]` re-enables it through a transitive path. See [Cargo Workspace and the Feature Unification Pitfall](https://nickb.dev/blog/cargo-workspace-and-the-feature-unification-pitfall/) and [issue #11329](https://github.com/rust-lang/cargo/issues/11329).

2. **How to PROVE wizard binary doesn't contain MCP beyond `nm | grep mcp`** — `nm` is necessary but insufficient. Four-layer verification required:
   - **L1 — Source-side**: `grep -rnE '#\[cfg\(feature\s*=\s*"mcp"\)\]|use .*mcp' crates/wizard-bin/ crates/wizard-*/` returns nothing (workspace fork should not even reference an `mcp` symbol from the wizard's reachable set).
   - **L2 — Symbol-side**: `nm -D --defined-only target/release/osagent-wizard | grep -Ei 'mcp|model[_]?context[_]?protocol' | grep -v ' U '` is empty. Use `--defined-only` so we don't false-positive on dynamic relocations.
   - **L3 — Linker-side, after LTO+strip**: `cargo bloat --release --bin osagent-wizard --crates | grep -i mcp` is empty AND `cargo bloat --release --bin osagent-wizard --filter mcp -n 10` returns no rows. `cargo-bloat` reads the actual symbol size attribution post-LTO, so it catches a function that ended up inlined into something else.
   - **L4 — String-side**: `strings target/release/osagent-wizard | grep -Ei 'mcp|stdio_mcp_server|sse_mcp' | head` is empty. Stripped symbols still leave format strings, log messages, and serde-derived string constants behind — if `"mcp"` appears in a log line or a `serde(tag = "mcp")`, the symbol gate passes but the code is in there.
   - All four are CI gates. Phase 1.4 wires them.

3. **Quarterly upstream merge discipline that doesn't degenerate** — six rules:
   - **Pin upstream by commit SHA, not tag or branch.** Tag `v0.7.6` can be re-cut; SHA is immutable. Class #10 (upstream installer pin-drift) generalises: the fix is "address by content hash, not human label".
   - **One merge PR per quarter; no cherry-picks between merges.** Cherry-picking individual upstream fixes mid-quarter creates conflict cascades that re-litigate at the next merge.
   - **`upstream-tag-N` CI suite runs against every merge PR** (decision #23 already covers this). Must include the 4-layer MCP gate above — a benign upstream refactor can move MCP code into a shared crate that wizard pulls in.
   - **Pre-merge diff-stat budget**: if upstream changed >2000 LOC in crates we keep, the merge gets a **per-file walk** review, not a "trust CI" review. Fork sustainability dies at the third quarter where reviewers stop reading the diff.
   - **Conflict-resolution log lives in `UPSTREAM_SYNC.md`** as an append-only table: `commit | upstream-SHA | conflicts-in-files | resolution-pattern`. Three quarters in, this table tells you what the structural divergence shape is — that's the early-warning that says "rebase the fork onto a cleaner subtree split before the next merge".
   - **Strategy: `git subtree merge`, not submodule.** Subtree puts all code in our tree (matches our drop-the-attack-surface goal — we ship one binary, not a binary+downloader), and matches what the Rust project itself does for tools depending on compiler internals. Submodule adds a runtime "where did upstream go" indirection that bites under air-gapped customer deployments. See [About Git subtree merges](https://docs.github.com/en/get-started/using-git/about-git-subtree-merges) and [rustc-dev-guide external-repos](https://rustc-dev-guide.rust-lang.org/external-repos.html).

4. **MANIFEST.toml discipline that doesn't lie** — three rules:
   - **MANIFEST.toml is emitted by the SAME build step that compiles the binary, from the SAME `#[cfg(feature = ...)]` source-of-truth that the binary consumed.** Forbidden: a separate `gen-manifest.sh` that parses `Cargo.toml`. That's class #1 (parallel-script divergence) waiting to happen — Cargo.toml says feature `mcp` is off, build.rs emits "mcp: false", actual binary linked it in via transitive unification (see #1 of these answers).
   - **`build.rs` writes MANIFEST.toml using the compiler's `CARGO_FEATURE_*` env vars** — those are what rustc actually saw, not what we wrote in Cargo.toml.
   - **`osagent manifest --diff config.toml` runs in install-guide preflight (ExecStartPre).** The manifest declares what the binary CAN do; the config declares what we WANT it to do; the diff fails fast if config asks for a feature the binary doesn't have, or — more importantly — if the binary contains a feature the config didn't authorise. Symmetric check, both directions.
   - **MANIFEST.toml is hash-anchored.** SHA256 of MANIFEST is computed in build.rs and `osagent --version` prints both the binary version AND the MANIFEST hash. Witness anchors this daily alongside the audit-chain hash (matches decision #39). A swapped-out MANIFEST on disk gets detected within 24h.

5. **Rust-fork-specific gotchas hitting community in 2025-2026** — five worth flagging:
   - **Implicit-features deprecation (RFC 3491)** — optional dependencies that were referenced as features-by-name without the `dep:` prefix are deprecated and will be removed in a future edition; long-lived forks with `optional = true` deps must migrate to `dep:` syntax or pin to a stable edition. See [RFC 3491](https://rust-lang.github.io/rfcs/3491-remove-implicit-features.html).
   - **Resolver v2/v3 mid-fork shift** — upstream may switch resolver versions in a quarterly merge; resolver change silently changes which features unify. Mandatory `resolver = "2"` (or "3" once stable) PIN in workspace Cargo.toml and CI assertion. See [workspace feature-unification tracking issue #14774](https://github.com/rust-lang/cargo/issues/14774).
   - **Rust 2024 edition behavioural changes** — `cargo fix --edition` is conservative; `impl Trait` lifetime capture, `let chains`, async hygiene, and ref-binding-on-`&Option<T>` changed semantics. A fork that didn't migrate when upstream did inherits a `cargo fix` debt that grows quarterly. See [Rust 1.85 + 2024 edition release notes](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/) and the [codeandbitters migration writeup](https://codeandbitters.com/rust-2024-upgrade/).
   - **Non-deterministic DCE in incremental builds** — [rust #150462](https://github.com/rust-lang/rust/issues/150462) tracks observed non-deterministic dead-code elimination, meaning two CI runs with identical inputs can produce binaries with subtly different stripped symbol sets. Implication for the `nm | grep mcp` gate: a passing CI run is not a binding proof for a different machine. Mitigation: `CARGO_INCREMENTAL=0`, `codegen-units = 1`, `lto = "fat"` in `[profile.release-locked]`, and run the gate in that profile only.
   - **`inventory`/`linkme` registry crates** — if zeroclaw uses either to auto-register channels/providers/tools, monomorphization sees the trait-object cast as a use-edge, so the vtable for every registered type gets instantiated regardless of feature flags. The dead one only gets stripped if BOTH `lto = "fat"` AND `--gc-sections` see no remaining reference. See [traitcast / inventory note](https://crates.io/crates/cast_trait_object_macros) and [rustc-dev-guide monomorph collector](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_monomorphize/collector/index.html).

---

## Critical Pitfalls

### Pitfall 1: Cargo workspace feature unification silently re-enables MCP on wizard

**What goes wrong:**
The wizard binary is built with `cargo build --bin osagent-wizard --features wizard-bin --no-default-features`, the wizard crate explicitly does NOT enable `mcp`, yet `nm osagent-wizard | grep -i mcp` returns symbols. The shared `osagent-core` crate (or one of its transitive deps) has `mcp` in its default feature set, so Cargo unifies the build and `mcp` ends up enabled across the whole graph.

**Why it happens:**
Cargo workspace feature unification: when a dependency is reachable from multiple workspace members, Cargo builds it once with the UNION of all features. `--no-default-features` on the wizard binary does NOT propagate to transitive dependencies — it only applies to the package being built directly. Documented in [cargo #1886](https://github.com/rust-lang/cargo/issues/1886) and [#8366](https://github.com/rust-lang/cargo/issues/8366); resolver v2 mitigates platform-specific cases but does not eliminate the workspace-member unification.

**How to avoid:**
- **Workspace `resolver = "2"` mandatory** (CI assertion via `grep -q '^resolver = "2"' Cargo.toml`).
- **No `default-features` on any shared crate.** All features explicit, opt-in. `[features] default = []` everywhere.
- **`mcp` lives ONLY in `osagent-mcp` crate** (its own crate, never re-exported from shared core). `osagent-engineer-bin` depends on `osagent-mcp`; `osagent-wizard-bin` does not. Wizard's Cargo.toml has zero `mcp` references.
- **Wizard build uses `--locked` and a separate Cargo.lock-equivalent** via `cargo --config 'profile.release.lto = "fat"'` to force full whole-program analysis.
- **CI gates** (4-layer, see top-of-document answer #2).

**Warning signs:**
- `cargo tree --format '{p} {f}' -e features --workspace` shows the `mcp` feature listed under any crate reachable from `wizard-bin`.
- `cargo metadata --format-version 1 | jq '.resolve.nodes[] | select(.id | contains("wizard-bin")) | .features'` lists `mcp`.
- `cargo bloat --release --bin osagent-wizard --crates` shows the `osagent-mcp` crate at all (any non-zero size = leak).

**Phase to address:** **1.4 (Strip dead features)** — establishes the feature-gating discipline. CI gate (L1+L2+L3+L4 in answer #2) wires up here. **1.2 (Workspace restructure)** establishes the per-binary feature names; **1.5 (MANIFEST.toml)** verifies the gate held at build emission time.

---

### Pitfall 2: `#[cfg(feature = "x")]` typos compile silently (no feature actually exists)

**What goes wrong:**
A `#[cfg(feature = "wizard_bin")]` (underscore vs hyphen) instead of `#[cfg(feature = "wizard-bin")]`. The block is always-false because no such feature exists. Code that should compile in is silently absent. Or worse — `#[cfg(not(feature = "mcp_disabled"))]` instead of `#[cfg(feature = "mcp")]` flips the gate. Wizard ships with mcp compiled in, CI `nm` gate passes because the symbol was renamed in an upstream merge but our gate still greps the old name.

**Why it happens:**
Pre-1.79 rustc silently treated unknown features as always-false. Newer rustc warns "unexpected cfg condition value" but **only if** features are declared via `cargo::rustc-check-cfg` or the new `[lints.rust.unexpected_cfgs]` setup. A fork that didn't enable `--check-cfg` inherits the silent-false behaviour. See [conditional-compilation-checking RFC 3013](https://rust-lang.github.io/rfcs/3013-conditional-compilation-checking.html).

**How to avoid:**
- **Enable `--check-cfg` workspace-wide** via `[lints.rust] unexpected_cfgs = { level = "deny", check-cfg = [...] }` in workspace `Cargo.toml`. Any typo'd feature name is now a build error.
- **The `nm` CI gate matches NORMALISED forms**: `nm osagent-wizard | grep -iE 'mcp|model[_-]?context[_-]?protocol|stdio_mcp|sse_mcp'`. Cover both naming conventions in upstream and any rename.
- **MANIFEST.toml lists every declared feature** AND every cfg-condition the build saw. A new feature added by an upstream merge that wizard doesn't reference shows up in MANIFEST as `present-but-unused = true` — auditable.

**Warning signs:**
- `cargo check --workspace --all-targets` emits `unexpected cfg condition value` warnings (or build errors if `deny`).
- A feature exists in `Cargo.toml` but no `#[cfg(feature = "x")]` references it anywhere — `grep -rnE '#\[cfg\(feature\s*=\s*"x"\)\]' crates/` returns empty.
- Inverse: a `#[cfg(feature = "x")]` references an undeclared feature.

**Phase to address:** **1.4 (Strip dead features)**. The `--check-cfg` setup is a 5-line addition to workspace Cargo.toml; do it on day one of the phase.

---

### Pitfall 3: `nm` symbol-grep gate passes but code is actually compiled in (inlined / monomorphized vtable)

**What goes wrong:**
`nm osagent-wizard | grep -i mcp` returns empty. CI passes. Wizard binary actually contains MCP server initialisation code — just inlined into a `start_subsystems()` function via `#[inline(always)]` and aggressive LTO. The vtable entry for `dyn ChannelHandler` includes an `McpHandler` variant because the registry-crate trait-object cast was reachable from a non-feature-gated `match` arm.

**Why it happens:**
`nm` lists symbols at link time. LTO can inline a function and erase its symbol name; the bytes are in `.text` under the parent's name. Monomorphization treats a trait-object cast as a use-edge regardless of cfg gates downstream, so the vtable gets instantiated. `strip = "symbols"` (default for `lto = true` profiles) makes this worse — local symbols vanish.

**How to avoid:**
- **4-layer verification** (answer #2 above): source-grep + `nm` + `cargo-bloat --filter` + `strings`. Each catches a different leak path.
- **`cargo bloat --release --bin osagent-wizard --crates`** is the load-bearing check. It reads post-LTO symbol attribution from the binary's `.text` section by walking debuginfo, so inlined code is still attributed to its origin crate. If `osagent-mcp` shows ANY non-zero size, the gate fails.
- **Treat trait-object registries specially.** If zeroclaw uses `inventory` or `linkme` for channel/provider/tool registration: the registration call site is the leak, NOT the implementation. Audit `inventory::submit!` and `linkme::distributed_slice` invocations; wrap every one in `#[cfg(feature = "x")]`.
- **`#[no_mangle]` ban on shared crates.** A `#[no_mangle]` function survives LTO regardless of reachability and shows up in `nm`. If a `#[no_mangle] pub extern "C" fn start_mcp(...)` survives, the symbol gate catches it; if upstream removes the `#[no_mangle]` in a merge, the gate goes from catching-it to missing-it without anyone editing the gate.
- **Build with `RUSTFLAGS="--cfg debug_assertions=\"feature_audit\"" -Cdebuginfo=2`** in a SECOND CI build whose only purpose is feeding `cargo-bloat`. Strip is off, symbols intact, attribution accurate.

**Warning signs:**
- `cargo bloat --release --bin osagent-wizard --crates` shows the `osagent-mcp` crate (or any MCP-related crate) at any size > 0.
- `strings target/release/osagent-wizard | grep -i mcp` returns log/format strings.
- Binary size of `osagent-wizard` is within 5% of `osagent-engineer` — wizard should be substantially smaller; if it's not, code we thought we stripped is still in there.

**Phase to address:** **1.4 (Strip dead features)** for the gate wiring. **1.3 (Strip dead crates)** establishes the size-budget baseline so we can detect leak via size delta in 1.4.

---

### Pitfall 4: Upstream installer pin-drift (CARRY-FORWARD from class #10)

**What goes wrong:**
zeroclaw's upstream `install.sh --prebuilt` queried `/releases/latest` regardless of git pin. Already lived through this on 2026-04-22 when upstream published v0.7.3 and our v0.6.8-pinned install silently shipped v0.7.3, breaking the engineer. osAgent inherits this risk both for itself (if we ship an `install.sh`) AND for its OWN dependencies that may carry similar scripts.

**Why it happens:**
A `git:` task pin only controls source-tree state; an in-source install script that calls a GitHub API is a runtime escape hatch from the pin. Class #10 in `sovereign-shield-install-guide/CLAUDE.md`. WE ALREADY FIXED THIS by direct tarball + SHA256; osAgent must continue this pattern AND extend it to:
- The osAgent release artifact itself (signed binaries land in `sovereign-shield-backup`, fetched by SHA256, not "latest").
- Any `build.rs` that fetches network resources (forbidden in offline / air-gap customer deployments; CI lint must reject network access from build.rs).

**How to avoid:**
- **No `install.sh` in osAgent**. Install is the ansible task `install_osagent.yml` that does `get_url` + `checksum: sha256:<hash>` from our own infra (decision #40 names self-hosted runner for signed releases).
- **`build.rs` lint**: CI gate that runs `cargo +nightly metadata` AND greps every `build.rs` in the workspace for `reqwest|ureq|curl|wget|hyper|fetch` — fail-fast if any build script can do network I/O.
- **`--version` reports the SHA256 of the binary itself** AND of the MANIFEST.toml shipped beside it. The ansible install task asserts `osagent-engineer --version | grep -q "$EXPECTED_SHA256"` post-install. Class #10's lesson generalised: never infer state from upstream; always verify content.

**Warning signs:**
- `grep -rn 'releases/latest' .` returns anything in osAgent or in any kept dependency.
- `cargo metadata --format-version 1 | jq '.packages[] | select(.metadata.network == true)'` (hypothetical lint) is non-empty.
- ansible install task uses `command: install.sh` instead of `get_url` + `checksum:`.

**Phase to address:** **1.1 (Fork + attribution + upstream-sync runbook)** for the policy; **1.6 (Install-guide drop-in task)** for the `get_url` + `checksum:` task definition.

---

### Pitfall 5: Live-vs-repo script drift (CARRY-FORWARD from class #7)

**What goes wrong:**
osAgent's `install_osagent.yml` ansible task is edited but not re-deployed. The change passes CI (lint, ansible-lint), passes a fresh-install test against a built test box, but doesn't propagate to the existing customer's running install until they happen to re-apply. Or: the engineer's life-config `scheduled/*.yaml` for an osAgent-specific tool is edited in `ola-host-engineer-config`, but the engineer keeps running the old shell-tool path because the binary's tool-allowlist is baked in at build time.

**Why it happens:**
Class #7 in install-guide CLAUDE.md. Install-time copies (e.g., `/etc/zeroclaw/osagent.toml`, `/usr/local/bin/{engineer,wizard}`) are point-in-time snapshots. The repo source updates without a re-apply do NOT propagate. Compounds for osAgent because:
- Two delivery surfaces (binary at `/usr/local/bin/` AND config at `/etc/zeroclaw/` AND MANIFEST at `/usr/local/share/osagent/MANIFEST.toml`) all need re-deploy on any change.
- MANIFEST hash check in install-guide preflight fails if MANIFEST drifts from binary (good — that's the gate), but ONLY if both got re-deployed; if neither got re-deployed, the diff against the old config is the actual silent bug.

**How to avoid:**
- **`install_osagent.yml` handlers chain**: `binary changed` triggers `daemon-reload` + `service restart` (covers class #2). `config changed` triggers `osagent manifest --diff` preflight + service restart. `MANIFEST changed` triggers `osagent --version` assertion. Every change has a handler; no change is silent.
- **`lint_script_drift.sh` runs in install-guide preflight** (already exists per class #7); extend to osAgent paths: assert `/usr/local/share/osagent/MANIFEST.toml` SHA256 matches `roles/osagent/files/MANIFEST.toml` SHA256.
- **`osagent --version` post-install assertion** in ansible task asserts the binary version + MANIFEST hash match what we just deployed.

**Warning signs:**
- `diff <(osagent-engineer --version) <(cat /opt/sovereign-shield/version-of-deployed-binary.txt)` is non-empty.
- A customer reports "the new feature works locally but not on prod" → didn't re-apply ansible.
- `osagent manifest --diff /etc/zeroclaw/config.toml` shows config asking for a feature the deployed binary doesn't have.

**Phase to address:** **1.6 (Install-guide drop-in task)** — handler chain definition. **1.5 (MANIFEST.toml)** for the hash-anchor preflight.

---

### Pitfall 6: Wizard binary inherits engineer's surface via shared crate

**What goes wrong:**
We split into `osagent-engineer-bin` and `osagent-wizard-bin`, both depending on shared `osagent-core`. Engineer needs a tool registry; the trait-object registry lives in `osagent-core`. Every tool variant (including ones wizard shouldn't have, e.g., `shell_tool`, `mcp_handler`) gets instantiated as part of the registry's vtable. Wizard ELF contains code for tools it never invokes because the registry pulled them in.

**Why it happens:**
Shared crate must compile against the union of features used by both binaries. If `osagent-core` exposes `pub enum Tool { Shell, Mcp, ChannelSend, VaultWrite }`, that enum is monomorphized in both binaries even if one variant is never constructed at runtime. The `match` exhaustiveness check forces the dead arms to compile.

**How to avoid:**
- **Crate-level split, not enum-variant split.** Each tool category is its own crate. Engineer-only tools (`osagent-tool-shell`, `osagent-tool-mcp`) are NOT in `osagent-core`. `osagent-engineer-bin` lists them as deps; `osagent-wizard-bin` does not.
- **Trait-object registries use generics, not enums.** `dyn ToolHandler` trait object is constructed only from concrete types the binary actually depends on. The registry crate exposes `register_tool<T: ToolHandler>(t: T)`; each binary's `main` registers its own set. No central `enum Tool` exists.
- **Sealed traits prevent downstream re-registration**: `trait ToolHandler: private::Sealed`. A vendored upstream merge can't sneak a new variant in without touching the sealed marker, which is a visible diff.
- **Coverage check**: `cargo bloat --release --bin osagent-wizard --filter '^osagent_tool_'` lists only `osagent_tool_vault_write` and the small set of wizard-allowed tools; engineer-only tool crates appear at zero size.

**Warning signs:**
- `cargo tree --workspace -i osagent-tool-shell` shows `osagent-wizard-bin` in the reverse-dep list.
- `cargo bloat --release --bin osagent-wizard --crates` shows engineer-only crates with non-zero size.
- A central `enum Tool` exists in `osagent-core` covering both binaries' tool sets.

**Phase to address:** **1.2 (Workspace restructure with 2 binaries)** — crate split decision happens here. **1.4 (Strip dead features)** verifies the result.

---

### Pitfall 7: Quarterly upstream merge degenerates ("conflict fatigue")

**What goes wrong:**
Q1 merge: clean, 200 LOC conflicts, 1 hour. Q2: 800 LOC, 4 hours. Q3: 2500 LOC, two days, reviewer skims because too much to read. Q4: a wholesale upstream refactor lands silently in a "trust CI" merge, and a year later we discover MCP was re-routed through a new shared module — wizard now ships MCP code, CI's `nm` gate misses the renamed symbol.

**Why it happens:**
Fork-divergence compounds. Every modification we make to a shared file makes upstream's next edit-in-the-same-file a conflict. Without active inverse-pressure (rebasing the FORK onto a cleaner subtree split, deleting our shims where upstream caught up), conflicts grow monotonically. Reviewers cope by reading less.

**How to avoid:**
- **Diff-stat budget per quarter**. >2000 LOC upstream churn in kept crates → mandatory per-file review (not "trust CI"). <2000 → CI + spot-check.
- **`UPSTREAM_SYNC.md` log** (decision #23 + the runbook) lists `quarter | upstream-SHA | LOC-conflict | files-touched | resolution-pattern`. Quarter 3 of consistent conflicts in the same file = signal that file needs a refactor on OUR side (extract our changes into an osAgent-only crate that wraps upstream cleanly).
- **CI suite `upstream-tag-N` runs the FULL 4-layer MCP gate** + the full ansible install on a clean VM. Not just unit tests.
- **"Refuse to merge" criteria**: any of (a) MCP gate fails, (b) MANIFEST.toml diff shows new feature added without an explicit decision-log entry, (c) Cargo.lock resolver version changed without an explicit migration step.
- **Subtree merge, not submodule** (see top-of-document answer #3 reasoning).

**Warning signs:**
- `UPSTREAM_SYNC.md` shows the same file in `files-touched` for 3+ consecutive quarters.
- Quarterly merge PR description gets shorter over time (reviewer copium).
- `git log --merges --oneline | head -4` shows only one author across all upstream-merge commits (no second review).

**Phase to address:** **1.1 (Fork + attribution + upstream-sync runbook)** — runbook IS this discipline.

---

### Pitfall 8: MANIFEST.toml lies (build-time vs runtime drift)

**What goes wrong:**
MANIFEST.toml claims `tools = ["vault_write", "channel_send"]`. Binary actually contains code for `shell_tool` (compiled in via a feature-unification leak per Pitfall 1). Install-guide preflight runs `osagent manifest --diff config.toml`, config says `tools = ["vault_write", "channel_send"]`, MANIFEST agrees, preflight passes. The binary actually exposes more than the MANIFEST claims — config validation gives us false security.

**Why it happens:**
MANIFEST is a sibling artifact to the binary. If MANIFEST is generated by parsing Cargo.toml (or any "what we declared") instead of what rustc actually compiled in (via `CARGO_FEATURE_*` env vars and the actual reachable symbol set), it's the WISH not the OUTCOME. Class #1 (parallel-script divergence) generalised: MANIFEST and the binary are parallel artifacts; both must be derived from the same source-of-truth.

**How to avoid:**
- **build.rs writes MANIFEST.toml from `CARGO_FEATURE_*` env vars**, AND additionally from `cargo-bloat`-style post-link analysis (the build emits MANIFEST AFTER the binary is linked, so it can include the actual symbol-set summary).
- **Two MANIFEST sections**: `[declared]` (what features Cargo enabled) AND `[detected]` (what symbols actually survived link). `osagent --version` prints both. A diff between them at build time is itself a CI failure.
- **MANIFEST hash anchored daily to witness** (matches decision #39, hash-chain to witness). Tamper detection at the FS layer.
- **`osagent manifest --diff config.toml` is BIDIRECTIONAL**: fails if config asks for what binary doesn't have (boring case) AND fails if binary has features config didn't authorise (security case).
- **No `gen-manifest.sh` standalone script.** MANIFEST and binary always co-emit from one `cargo build`.

**Warning signs:**
- Two CI runs produce MANIFEST.toml files with different content (non-deterministic — see [rust #150462](https://github.com/rust-lang/rust/issues/150462) on non-deterministic DCE). Reproducibility lost = MANIFEST untrustworthy.
- MANIFEST `[declared]` and `[detected]` sections disagree.
- A `gen-manifest.sh` script exists at all (architectural smell).

**Phase to address:** **1.5 (Telemetry audit + strip + MANIFEST.toml)** — owns the MANIFEST design.

---

### Pitfall 9: Service restart after EnvironmentFile change missed (CARRY-FORWARD from class #2)

**What goes wrong:**
osAgent's ansible task adds a new env var to `/etc/sovereign-shield/osagent-engineer.env`. The systemd unit's `EnvironmentFile=` points at this file. We don't `daemon_reload: yes` + `state: restarted`. The running engineer doesn't pick up the change. Either nothing happens (silent miss) or the change is partial (some processes read it, the daemon doesn't).

**Why it happens:**
Class #2 directly applies. `daemon-reload` alone re-reads unit files; running daemons keep their in-memory env. `EnvironmentFile=` is read at unit start, not at daemon-reload. Compounds with class #5 (invisible-on-rerun): change passes on already-installed systems because they restart at next boot.

**How to avoid:**
- **Ansible handler chain**: `osagent-engineer.env` content change → notify `restart-osagent-engineer` handler. Handler does `daemon_reload: yes` + `state: restarted`.
- **Post-restart assertion**: `journalctl -u engineer -n 20 --since "10 seconds ago"` shows the new env in the engineer's "config loaded" log line. Class #14 (`|| true` ban) compounds — the assertion is fail-fast, no swallowing.
- **OS env var change ALSO triggers MANIFEST diff check** (per Pitfall 8) — if the env var asked for a feature the binary doesn't have, the diff catches it pre-restart.

**Warning signs:**
- ansible task modifies env file without `notify:` to a restart handler.
- `systemctl show engineer -p Environment` after deploy doesn't include the new var.

**Phase to address:** **1.6 (Install-guide drop-in task)**.

---

### Pitfall 10: Invisible-on-rerun bug class (CARRY-FORWARD from class #5)

**What goes wrong:**
osAgent install task works perfectly when applied on a server that ALREADY has zeroclaw v0.7.5 installed (because cutting over reuses existing state — env files, certs, sandbox setup all already in place). It silently fails on a clean ubuntu VM because something we didn't realise was a precondition wasn't being established by our task.

**Why it happens:**
Class #5 directly. Common variants for osAgent:
- We assume the `engineer` / `wizard` system users exist (zeroclaw created them) → fresh install fails at chmod.
- We assume `/etc/zeroclaw/` exists → fresh install fails at copy.
- We assume `/var/log/osagent-audit.log` is touchable by the engineer user → fails per class #22 (pre-create log files for non-root daemons).
- We assume RMQ vhost `shield_internal` + user `engineer` already provisioned → bridge handshake fails (class #21 also bites).
- The "production migration via sharp cutover" decision (#30) makes this WORSE: customer's first osAgent install IS the existing-zeroclaw upgrade path; our coverage of "clean install" is purely the test box.

**How to avoid:**
- **Mandatory clean-VM CI test**. Reference-server snapshot is a known-state baseline, but `install_osagent.yml` runs in CI against a fresh `ubuntu:24.04` container with NOTHING pre-installed and asserts end-to-end success including `systemctl status engineer wizard`.
- **Belt-and-braces preconditions in the task**: `user: name=engineer state=present`, `file: path=/etc/zeroclaw state=directory owner=root group=root mode=0755`, `file: path=/var/log/osagent-audit.log state=touch owner=engineer group=engineer mode=0640 modification_time=preserve access_time=preserve` (class #22 pattern). All idempotent on re-run, all load-bearing on fresh.
- **Smoke gate at Phase 3 EXIT** asserts both binaries respond to a stub probe.

**Warning signs:**
- Task uses `file: state: touch` without `modification_time: preserve` (would re-touch on every run, breaks idempotency).
- "It works for me" reported by someone testing on a reference-server snapshot; "it doesn't work" on the customer's fresh server.

**Phase to address:** **1.6 (Install-guide drop-in task)** — clean-VM CI test definition.

---

### Pitfall 11: Workbench ownership contamination (CARRY-FORWARD from class #11)

**What goes wrong:**
osAgent's engineer (running as `engineer` user) has a workbench at `/home/engineer/.zeroclaw/workspace/workbench/`. Admin opens root shell, runs `git status` in a workbench repo. Git re-owns `.git/index`, `.git/HEAD`, etc. as `root:root`. Engineer's next read hits EACCES — agent reports "security policy blocks this", administrator chases red herring for two hours.

**Why it happens:**
Class #11. CAP_DAC_OVERRIDE on root ignores mode bits. Already lived through this on 2026-04-23. osAgent's engineer is in the same role and inherits the threat surface.

**How to avoid:**
Three-layer defense unchanged (class #11 spec):
1. **Prevent** — `engg` / `wizz` wrappers + `/root/.bashrc.d/workbench-guard.sh` (already deployed).
2. **Heal** — `chown-enforcer.timer` every 5 min (already deployed).
3. **Detect** — `scripts/lint_workbench_ownership.sh` (already deployed).

osAgent's bridge tool (the new native Rust tool replacing the shell+python3 path per the decisions table) MUST NEVER chown the workbench tree from the agent's process. The agent runs as `engineer`; chmodding files it owns is fine, chowning is a CAP_CHOWN escalation it shouldn't have. If the bridge tool somehow gets CAP_CHOWN (via setcap, by misconfiguration), it can corrupt its own workbench under attacker control.

**Warning signs:**
- Bridge tool source contains `chown()` or `nix::unistd::Uid::set()` calls.
- Bridge binary has `cap_chown=ep` in `getcap`.
- Test: `sudo find /home/engineer/.zeroclaw/workspace/workbench -not -user engineer | head` returns rows post-test.

**Phase to address:** **1.2 (Workspace restructure)** establishes that the engineer-amqp-bridge is a native Rust crate with no `chown` capability. CI gate (`grep -rE 'chown|set_uid' crates/engineer-bridge/`) ensures it stays that way.

---

### Pitfall 12: Cross-UID writer ownership drift (CARRY-FORWARD from class #12)

**What goes wrong:**
osAgent introduces a config-refresh path (if we add one — e.g., `osagent config refresh` pulling updated allowlists from `ola-host-engineer-config`). Refresh runs as root via systemd timer, writes into a bind-mounted `.git/` tree. The container-UID consumer (persist-service, witness's audit-scrape, etc.) silently loses access to its own repo because `.git/*` got re-owned to `root:root`.

**Why it happens:**
Class #12, the automation-path variant. Stealthier than #11: no interactive shell, no banner. Lived through this 2026-04-24 with persist-service exporter goroutine.

**How to avoid:**
Same defense as class #12:
- Read outer-dir ownership at runtime with `stat -c '%u:%g'` and chown `.git` recursively to that owner after every root-side write.
- Don't hard-code the UID; couples phases.
- Failure of chown is non-fatal but logged as WARN.

For osAgent: if there's a config-refresh path that touches anything bind-mounted, this applies. **If there's NOT** (config is pull-only, no host-side fetch), document that explicitly in the architecture so a future "let's add hourly refresh" PR re-evaluates the gate.

**Warning signs:**
- osAgent install task creates a systemd timer that runs as root and touches a path also mounted into a container.
- `grep -rE 'git fetch|git reset|git pull' ansible/files/osagent-*.sh` returns rows without a following chown-back.

**Phase to address:** **1.6 (Install-guide drop-in task)** — if a config-refresh timer is added. If not, **document the non-decision** in the runbook so it's revisited.

---

### Pitfall 13: Daemon-restart-without-dependent-recovery (CARRY-FORWARD from class #13)

**What goes wrong:**
osAgent's install task restarts dockerd (very unlikely — but conceivable if osAgent needs to pull a docker-driven sandbox or local LLM container). dockerd restart without `live-restore` SIGTERMs every container; install reports `failed=0`; the entire compose stack is down.

**Why it happens:**
Class #13. Lived through 2026-04-27.

**How to avoid:**
- osAgent's install task **does NOT restart dockerd**. Period. Document this as a constraint.
- If a docker-side action is needed (pull image, etc.), it goes through `docker compose pull` / `docker compose up -d` only; no daemon restart.
- If a daemon restart IS unavoidable, the class #13 invariant applies in full: `live-restore: true` in initial daemon.json, recovery per compose project, post-recovery assertion, Phase 3 EXIT smoke gate.

**Warning signs:**
- `grep -rE 'systemctl.*restart.*docker|service docker restart' ansible/` returns rows in osAgent's tasks.

**Phase to address:** **1.6 (Install-guide drop-in task)** — constraint documentation.

---

### Pitfall 14: TLS-mandate / sibling-pattern enforcement (CARRY-FORWARD from class #18)

**What goes wrong:**
osAgent's bridge tool needs to talk AMQP to operator-service. It opens a `lapin::Connection` without TLS, because the dev/test setup used 5672. When deployed, 5672 is closed (mTLS-only 5671 per the install-guide pattern). Bridge fails to connect → engineer reports "operator unreachable". Hours of red-herring chasing.

**Why it happens:**
Class #18. Sibling-pattern enforcement: every Go service in sovereign-shield uses the same `tls.Config.ServerName = "rabbitmq.shield.internal"` pattern. A new Rust client written without copying the pattern repeats the bug.

**How to avoid:**
- **Shared `osagent-amqp` crate** holds the TLS bootstrap helper (CA file, cert file, key file, ServerName override). Both the bridge AND any other AMQP client in osAgent imports this helper. No per-service re-implementation.
- **Cert mirroring (class agent sandbox)**: bridge reads from `$HOME/.zeroclaw/certs/engineer-amqp/`, NEVER `/opt/sovereign-shield/certs/...` (sandbox denies). osAgent's ansible install task mirrors certs into `$HOME/.zeroclaw/certs/`, matching `configure_engineer_amqp.yml`.
- **CI lint**: `grep -rnE 'amqp://|lapin::Connection::connect\([^,]+,' crates/` — any unencrypted `amqp://` or `Connection::connect()` without a TLS config is a build error.

**Warning signs:**
- `cargo tree -i lapin` shows multiple direct dependents (instead of one helper crate).
- `strings target/release/osagent-engineer | grep -E 'amqp://[^s]'` returns non-empty (plain amqp:// URL embedded).

**Phase to address:** **1.2 (Workspace restructure)** — the shared `osagent-amqp` crate gets defined here.

---

### Pitfall 15: TLS listener verify race after broker restart (CARRY-FORWARD from class #21)

**What goes wrong:**
osAgent's bridge connects to RMQ at startup. If install-guide just restarted the broker (cert rotation, TLS flip, broker version bump), the listener takes 5-30 seconds to bind on cert-chain load. Bridge connects in that window, hits "connection refused" → engineer's `engineer.service` enters `Restart=always` loop → journal saturation (class #15 compounds).

**Why it happens:**
Class #21. systemd reports unit "active" when supervisor is up, not when the listener has bound. Class #5 (invisible-on-rerun) hides this until the next fresh-install.

**How to avoid:**
- **Bridge's AMQP connect uses retries with backoff**: 24 retries × 5s = 2min budget, exponential backoff up to that ceiling. Matches the install-guide's pattern.
- **`StartLimitIntervalSec=60 StartLimitBurst=3 StartLimitAction=none`** on `engineer.service` and `wizard.service` `[Unit]` blocks (class #15). The 2min retry budget should swallow normal startup race; the StartLimit catches genuinely-broken cases.
- **Post-install smoke probe** does the openssl s_client TLS handshake test against `127.0.0.1:5671` as the engineer user, asserting OK before declaring install done.

**Warning signs:**
- Bridge source has `lapin::Connection::connect(url).await?` without retry wrapping.
- `engineer.service` unit lacks `StartLimitIntervalSec`/`StartLimitBurst`.

**Phase to address:** **1.2 (Workspace restructure)** for the retry logic in `osagent-amqp` crate. **1.6 (Install-guide drop-in task)** for the systemd unit's StartLimit and the smoke probe.

---

### Pitfall 16: Pre-create audit log file for non-root engineer/wizard daemons (CARRY-FORWARD from class #22)

**What goes wrong:**
osAgent appends to a per-customer hash-chained audit log (decisions #22 + #39). The engineer/wizard daemon tries to `open(O_CREAT)` on `/var/log/sovereign-shield/osagent-audit-<customer>.log` at startup. `/var/log/` is `root:root 0755`. Engineer/wizard can't create the file. Daemon crashes or silently logs nothing (the latter is worse — audit gap).

**Why it happens:**
Class #22. Lived through 2026-04-28 with vault audit. Engineer/wizard run as their named users, not root.

**How to avoid:**
- ansible task pre-creates the audit log: `file: path=/var/log/sovereign-shield/osagent-audit-<customer>.log state=touch owner=engineer group=engineer mode=0640 modification_time=preserve access_time=preserve`. Same for wizard. `modification_time: preserve` keeps it idempotent.
- The audit-log path is under `/var/log/sovereign-shield/` (NOT under `/opt/sovereign-shield/` — sandbox blocks that prefix per the agent-sandbox invariant).
- **CI lint** on the install task: every `service: name={engineer,wizard}` task is preceded by a `file: state=touch` for the audit log.

**Warning signs:**
- `journalctl -u engineer | grep -i 'permission denied'` returns audit-log-open errors.
- Audit-log file doesn't exist at install time; engineer creates it (then it has wrong owner if engineer ran as root briefly).

**Phase to address:** **1.6 (Install-guide drop-in task)**.

---

### Pitfall 17: Implicit-features deprecation (RFC 3491) bites mid-fork

**What goes wrong:**
Upstream zeroclaw uses `optional = true` on dependencies AND references those deps as features by bare name (e.g., `[features] some-feature = ["some-dep"]` instead of `["dep:some-dep"]`). RFC 3491 deprecates this; in a future edition it's removed. A quarterly upstream merge that lands a Rust toolchain bump silently breaks our build because our fork inherited the implicit-feature usage.

**Why it happens:**
[RFC 3491 — remove implicit features](https://rust-lang.github.io/rfcs/3491-remove-implicit-features.html) (or its currently-tracked stabilization issue) is on the 202X-edition deprecation list. The change is mechanical but blanket.

**How to avoid:**
- **CI lint at fork time**: `grep -rE '^\s*[a-zA-Z_-]+\s*=\s*\["[a-zA-Z_-]+"' crates/*/Cargo.toml` and assert each name in the feature list either starts with `dep:` or matches a `[features]` entry. Fail-fast on bare optional-dep names.
- **Migrate to `dep:` prefix at fork time** as part of phase 1.4 — easier to do once than incrementally each quarter.

**Warning signs:**
- `cargo +nightly check --workspace` emits "implicit feature" deprecation warnings.

**Phase to address:** **1.4 (Strip dead features)** — bundle the `dep:` migration here.

---

### Pitfall 18: Resolver version mid-fork shift

**What goes wrong:**
Workspace `Cargo.toml` doesn't pin `resolver = "2"` (defaults to "1" for legacy workspaces, "2" for ones using edition 2021+, and resolver "3" is on the way). An upstream merge changes the edition or the resolver pin; feature unification rules shift; tests pass on dev, deployment ships a binary with subtly different feature set than CI verified.

**Why it happens:**
Resolver version controls how features unify. Resolver 1 unifies aggressively; resolver 2 splits dev/build/normal; resolver 3 (in flight per [cargo #14774](https://github.com/rust-lang/cargo/issues/14774)) adds workspace-level controls.

**How to avoid:**
- **Mandatory `resolver = "2"` pin** in workspace `Cargo.toml`. CI asserts via `grep -q '^resolver = "2"' Cargo.toml`.
- **Resolver version is on the CHANGELOG.md for the quarterly upstream merge.** If upstream changed resolver, that's a per-file review trigger (not "trust CI"), per Pitfall 7.
- **Lock `[workspace.resolver]`** alongside `[workspace.package.edition]`. Both are reviewed at every merge.

**Warning signs:**
- `Cargo.toml` has no `resolver = ` line at workspace level.
- `cargo metadata --format-version 1 | jq '.workspace_default_resolver'` (hypothetical) shows "1".

**Phase to address:** **1.2 (Workspace restructure)** — establish the pin.

---

### Pitfall 19: Non-deterministic dead-code elimination across CI runs

**What goes wrong:**
CI passes the 4-layer MCP gate. Engineer deploys to customer; on the customer's hardware, `nm` shows different stripped symbols. The 2026-released bug [rust #150462](https://github.com/rust-lang/rust/issues/150462) documents observed non-deterministic DCE in incremental builds. The CI assertion is not a binding proof for the production binary.

**Why it happens:**
Incremental compilation parallelism + LTO ordering can produce different stripped symbol sets on different machines. Compounds with `codegen-units > 1`.

**How to avoid:**
- **Release builds pin: `CARGO_INCREMENTAL=0`, `codegen-units = 1`, `lto = "fat"`, `strip = "none"` for the symbol gate**, then a separate `strip = "symbols"` pass for the deploy artifact.
- **Reproducibility CI**: run the release build TWICE on different runners, assert byte-equal binary (or symbol-set-equal post-strip). Failure = non-determinism leak.
- **The deploy artifact's SHA256 is computed by CI, signed by the self-hosted runner (decision #40), and ALL gate assertions are checked against THAT artifact** (not a re-build).

**Warning signs:**
- Two CI runs of the same commit produce different binary SHA256.
- `MANIFEST.toml[detected]` section differs between two runs.

**Phase to address:** **1.5 (Telemetry audit + strip + MANIFEST.toml)** — reproducibility CI wiring fits here.

---

### Pitfall 20: Telemetry strip incomplete — outbound metrics survive

**What goes wrong:**
TELEMETRY-01 audits zeroclaw for phone-home and strips outbound metrics. We catch the obvious `posthog.com`, `sentry.io`, etc. We miss: (a) anonymous PII-free crash reports baked into a dependency's default config; (b) `users.rust-lang.org`-style update checks in a transitive dep; (c) DNS lookups for telemetry domains that fail-closed but still leak the lookup itself to the customer's recursive resolver (audit log).

**Why it happens:**
Telemetry is often in dependencies, not the host code. `sentry`, `tracing-actix`, etc., bring their own HTTP clients. Even with the dep stripped, a transitive may have its own.

**How to avoid:**
- **Audit at the dependency graph level**: `cargo tree -e normal --workspace --no-default-features --features wizard-bin | grep -Ei 'sentry|posthog|opentelemetry|datadog|tracing-honeycomb|reqwest|hyper'`. Every HTTP client crate in the tree gets a justification entry in TELEMETRY_AUDIT.md.
- **Network-egress whitelist enforcement**: the ansible install task installs an iptables/nftables egress rule allowing `engineer.service` and `wizard.service` to talk ONLY to the documented set (RMQ broker, Vault, configured LLM providers per `provider_policy`, configured chat APIs). All other egress drops + logs. A telemetry attempt becomes visible as a dropped-packet log line.
- **Test under `unshare -n` (or equivalent network namespace) in CI**: the binary starts up with NO network and asserts no early-startup error. If anything assumes network-available at startup (telemetry init), that's the gate.

**Warning signs:**
- `cargo tree -e normal | grep -Ei 'sentry|posthog'` returns rows after Phase 1.5.
- `strings target/release/osagent-engineer | grep -E '(api|telemetry|track)\.[a-z]+\.(com|io|net)'` shows external hostnames.

**Phase to address:** **1.5 (Telemetry audit + strip + MANIFEST.toml)**.

---

### Pitfall 21: Embedded credentials in agent-reachable filesystem (CARRY-FORWARD from class #16)

**What goes wrong:**
osAgent's install task writes a `/etc/zeroclaw/osagent.toml` containing the engineer's AMQP password, or the wizard's Vault bootstrap token, in a path the engineer can `cat`. Engineer LLM gets prompted (e.g., red-team scenario) to "report your config" — it returns the secret. Or: a tool call (`shell_tool ls /etc/zeroclaw`) returns config-file paths and a follow-up `cat` exfiltrates.

**Why it happens:**
Class #16. Lived through 2026-04-28 with PAT in workbench `.git/config`.

**How to avoid:**
- **Secrets in env files at `/etc/sovereign-shield/osagent-engineer.env` (root:engineer 0640)** — engineer can READ via the systemd-provided env, but cannot `cat` the file (mode 0640 + the engineer's sandbox doesn't allow `/etc/sovereign-shield/` per the install-guide's pattern).
- **All other secrets in Vault, fetched at startup with the bootstrap secret.** Decision #7 already covers the bootstrap secret as sealed plaintext mode 0600 root:wizard.
- **CI lint on install task**: `grep -rnE '(password|token|secret|key)\s*=' ansible/files/osagent-*` is empty. Any password in a file template is a build error.
- **PAT-strip task** if osAgent ever clones a repo from inside the install: `regexp: 'url\s*=\s*https://[^@]+@github\.com/'` → `replace: 'url = https://github.com/'` (the same pattern as class #16).

**Warning signs:**
- `find /etc/zeroclaw /etc/sovereign-shield -type f -exec grep -lE 'password|token|secret' {} \;` returns engineer-readable files.
- `sudo -u engineer cat /etc/zeroclaw/osagent.toml` succeeds AND the file contains a secret.

**Phase to address:** **1.6 (Install-guide drop-in task)**.

---

### Pitfall 22: Workbench-vs-life-config trust boundary (CARRY-FORWARD from class #17)

**What goes wrong:**
osAgent's engineer can self-modify its own life-config (RAG docs, scheduled jobs, allowlist). Engineer commits a change → push → next hourly refresh → engineer's behaviour changes. Even with `engineer_config_refresh_enabled` defaulting to false (per the install-guide hardening), if we add an osAgent-specific path that bypasses this gate, we recreate the F-010 vulnerability.

**Why it happens:**
Class #17. Lived through 2026-04-28 — engineer self-modification via PAT in workbench `.git/config`.

**How to avoid:**
- **osAgent's bridge tool MUST NOT have a tool that writes to `/home/engineer/.zeroclaw/workspace/workbench/`-resident life-config repos** (`ola-host-engineer-config`, `ola-management-wizard-config`).
- **Per decisions: skill provenance signing (#27).** The engineer verifies skill catalog signatures BEFORE load. Wizard's signature is the trust root. If engineer's life-config arrives unsigned (because engineer wrote it itself), engineer refuses to load.
- **CI lint on engineer's tool list**: no tool whose effect is "write to a path matching `*ola-host-engineer-config*` or `*ola-management-wizard-config*`". Compile-time prohibition, not runtime.

**Warning signs:**
- Engineer's tool registry includes `git_push` or `write_to_workbench` for a life-config repo path.
- `engineer_workbench_repos` ansible var lists either life-config repo.

**Phase to address:** **1.2 (Workspace restructure)** — engineer's tool list is established here.

---

### Pitfall 23: License attribution drift on quarterly merge

**What goes wrong:**
Quarterly upstream merge adds new transitive dependencies. Their LICENSE files are downloaded but not aggregated into our `NOTICE` file. Customer audits the binary for license compliance, finds an MIT dep whose copyright notice isn't in our distribution. Compliance flag.

**Why it happens:**
The MIT license requires reproducing the copyright header for every MIT library in use. Apache-2.0 has explicit NOTICE-aggregation requirements. Long-lived forks accrue deps quarterly; manual NOTICE updates drift.

**How to avoid:**
- **`cargo about generate -c about.toml -o NOTICE` runs in CI** every release; output is committed alongside the binary. Auto-generated NOTICE catches new transitive deps.
- **`cargo deny check licenses`** in CI rejects any license outside an allowlist (MIT, Apache-2.0, BSD-3, ISC, MPL-2.0 with mode-of-use review). New copyleft dep = build failure.
- **NOTICE is signed alongside the binary** (decision #40 — self-hosted runner signs releases). Tamper-evident.

**Warning signs:**
- `cargo about generate` output differs from committed NOTICE.
- `cargo deny check licenses` warns or fails.
- A new transitive dep without a matching NOTICE entry.

**Phase to address:** **1.1 (Fork + attribution + upstream-sync runbook)** — NOTICE generation and license CI gate wired here.

---

### Pitfall 24: Provider-policy `local-only` silently falls back to cloud

**What goes wrong:**
Customer is on `local-only` provider policy. Oracle (local Ollama) is unreachable. Engineer's LLM call falls back to Anthropic / Gemini cloud, because the provider chain logic was implemented as "try local, on error try next provider". The customer's air-gap promise is broken silently.

**Why it happens:**
Generic LLM proxy code is written with fallback in mind (high availability). The constraint "local-only means refuse, never fall back" is a customer-specific stance, not a default. If we inherit upstream's `Provider::call_with_fallback()` and use it in `local-only` mode, the constraint is silently violated.

**How to avoid:**
- **Compile-time separation of provider chain logic**: `local-only` mode uses a DIFFERENT type (e.g., `LocalOnlyProvider`) that has NO fallback method. `cloud-first` and `local-first` use `FallbackProvider` which DOES. The type system enforces "you cannot call fallback in local-only mode".
- **`provider_policy = "local-only"` in config rejects any provider list that includes a cloud provider AT CONFIG-LOAD TIME.** Config validation refuses to start; no runtime check needed.
- **Alert + refuse-to-serve when oracle is unreachable in local-only mode.** Already in PROJECT.md constraints. Test: kill oracle, send a chat, assert the engineer's response is the refusal message, NOT a cloud-routed answer.

**Warning signs:**
- `grep -rE 'fallback|try_next|on_error_try' crates/osagent-providers/` reveals a fallback path reachable from `LocalOnlyProvider`.
- A `Vec<Provider>` field on the runtime config rather than separate-types-per-mode.

**Phase to address:** Out of M1 scope (provider chain ships M2+); but **document the architectural constraint in 1.2 (Workspace restructure)** so the M2 provider crate respects the type-level separation.

---

## Technical Debt Patterns

| Shortcut | Immediate Benefit | Long-term Cost | When Acceptable |
|----------|-------------------|----------------|-----------------|
| Use `#[cfg(feature = "x")]` instead of crate-level split for two-binary separation | Faster setup; one PR | Feature-unification leak (Pitfall 1), inlined-code leak (Pitfall 3), trait-object monomorphization leak (Pitfall 6); MCP can sneak back into wizard | **NEVER for the MCP/wizard separation.** Acceptable for stripping channels/providers within a single binary where security boundary doesn't matter |
| `gen-manifest.sh` standalone script instead of build.rs emission | Easier to iterate manifest format | Manifest lies (Pitfall 8); parallel-script divergence (class #1) | Never |
| Skip `--check-cfg` setup | Saves 10 lines of Cargo.toml | Typo'd feature names silently always-false (Pitfall 2) | Never |
| `default-features` left on for any shared crate | Less Cargo.toml clutter | Feature unification leaks (Pitfall 1); MCP gate becomes unreliable | Never |
| Quarterly merge via cherry-pick of upstream fixes between merges | Faster propagation of one specific fix | Quarterly merge becomes un-mergeable (Pitfall 7); reviewer fatigue | Never; if a single upstream fix is critical (security), do an out-of-cycle full merge, not a cherry-pick |
| `lto = "thin"` on release builds | Faster CI builds | Pitfall 3 (inlined-code leak) worsens; Pitfall 19 (non-determinism) worsens | Acceptable for dev/test builds; never for the security-gate build profile |
| `codegen-units > 1` on release | Faster CI | Same as `lto = "thin"`: non-determinism, less aggressive DCE | Never for the gate build; OK for non-gate test builds |
| Skip clean-VM CI test for install task | One less CI lane | Class #5 (invisible-on-rerun, Pitfall 10) bites first customer | Never |
| Share `osagent-mcp` crate with wizard "but feature-gate it" | Looks cleaner | Pitfall 1, Pitfall 6 reintroduce MCP | Never |
| Submodule for zeroclaw instead of subtree | Fewer files in our repo | Air-gap customer can't fetch submodule; class #10 (pin-drift) re-appears at submodule level | Never |

---

## Integration Gotchas

| Integration | Common Mistake | Correct Approach |
|-------------|----------------|------------------|
| AMQP (RMQ shield_internal vhost 5671) | Hardcoded plain `amqp://` URL; default `lapin::Connection::connect` without TLS | Shared `osagent-amqp` crate with TLS bootstrap helper; `AMQP_TLS_SERVER_NAME` env override (class #18); cert mirror to `$HOME/.zeroclaw/certs/engineer-amqp/` (sandbox-allowed) |
| Vault (writes from wizard) | Hardcoded Vault URL; no path enforcement; secret left in memory after write | Path assertion every write (`secret/data/<customer_id>/*`, decision #6); idempotency key = hash(tool + args + correlation_id) (decision #8); zeroize secrets post-write via `zeroize` crate |
| systemd (`engineer.service`, `wizard.service`) | Restart without `daemon-reload`; EnvironmentFile changed without restart | Class #2 handler chain; `StartLimitIntervalSec=60 StartLimitBurst=3` in `[Unit]` (class #15) |
| sandbox-allowed `$HOME` | Reading from `/opt/sovereign-shield/`-anchored path (sandbox blocks it) | Mirror into `$HOME/.zeroclaw/...` (class agent sandbox in install-guide); never weaken sandbox denial |
| OS-MDashboard chat-relay | Drop the `/ws/chat` endpoint thinking it's part of the stripped REST surface | KEEP `/ws/chat` + paired_tokens auth path (PROJECT.md STRIP-05 explicit) |
| Witness audit anchor | Audit chain hash computed but not anchored to witness | Daily anchor cross-link (decision #39); audit-chain hash AND MANIFEST hash both go to witness |
| Telegram / Slack / Matrix / Mattermost / WhatsApp-Cloud / Signal channels | Re-implementing TLS per channel client | Each channel's HTTP client uses the shared `osagent-http` crate with `cert=`/`verify=` baked in (class #18) |
| Engineer-amqp-bridge port | Hardcoded port; collides between engineer/wizard | Per-agent port (engineer 42617, wizard 42618 from prior session); `zeroclaw_gateway_ports` ansible var indexed by `agent_user` (class #8) |

---

## Performance Traps

(M1 is foundation; performance is not the goal. Listed for completeness.)

| Trap | Symptoms | Prevention | When It Breaks |
|------|----------|------------|----------------|
| `codegen-units = 1 + lto = "fat"` slows builds 10x | CI minutes spike | Run gate-build profile ONLY for security gate; dev/test use `codegen-units = 16 + lto = false` | Once CI runs > 20 min per PR, devs start skipping the gate |
| Quarterly merge accumulates dead-code we forgot to strip | Binary size grows quarter-over-quarter | Size-budget assertion in CI: `osagent-wizard < 40MB` (or whatever baseline post-strip is). Fails if binary grows | Once binary is > 100MB, MANIFEST diff is too noisy to read |
| Audit hash-chain compute is per-line synchronous | Engineer pauses 50ms per audit emit; LLM call throughput drops | Batch audit emit into 1s windows; chain over batch boundaries (decision #22 allows file sink, batching is implementation detail) | At >100 tool-calls/sec, latency visible to user |

---

## Security Mistakes

| Mistake | Risk | Prevention |
|---------|------|------------|
| MCP code reaches wizard binary via feature-unification | Wizard can exfiltrate Vault secrets through MCP arbitrary-code-execution | Pitfall 1 + 2 + 3; 4-layer CI gate |
| Secrets in agent-readable config file | Engineer exfiltrates secret via `cat` or LLM-prompted leak | Pitfall 21 — env file 0640, secrets in Vault, lint on install task |
| Engineer self-modifies life-config | Engineer alters its own constraints | Pitfall 22 — no engineer tool writes to life-config repos; provenance signature on load |
| Provider-policy `local-only` falls back to cloud | Air-gap promise broken silently | Pitfall 24 — type-level separation, no runtime fallback path reachable |
| Bridge tool has CAP_CHOWN | Bridge can corrupt workbench files | Class #11 + Pitfall 11 — bridge binary has no setcap; lint denies `chown` syscall in source |
| AMQP plaintext (5672) instead of TLS (5671) | Cleartext bridge traffic on host | Pitfall 14 — shared `osagent-amqp` crate; CI lint denies `amqp://` URL |
| Telemetry phone-home in a transitive dep | Outbound metrics leak customer fingerprint | Pitfall 20 — `cargo tree` audit, network namespace CI test, egress whitelist |
| Vault write without 2-person ack | Single-channel-compromise = secret rotation | Decision #5: 2-person ack hardcoded in Rust runtime, distinct identities verified |
| Vault write without idempotency key | Replay attack rotates secret twice | Decision #8: idempotency key = hash(tool + args + correlation_id) |
| Subagent depth > 1 | Fork-bomb / cost amplification | Decision #14: depth 1 enforced; runtime check refuses depth-2 spawn |
| Unsigned subagent prompt | Engineer runs attacker-supplied prompt | Decision #17: wizard signs; engineer verifies before invoke |
| Pause-gate semantics partial | Vault write completes mid-pause, half-state | Decision #21: Vault writes complete current transaction then halt; CancellationToken everywhere else |
| Codeword challenge bypassed | High-risk tool call without confirmation | Decision #10: 4-word codeword challenge in-channel; type-level requirement on high-risk tool struct |
| Channel outbox lost on disconnect | Customer-facing message dropped | Decision #13: SQLite per channel, replay on reconnect |
| `cloud-first` provider chain silently exfiltrates to non-customer-approved cloud | LLM call routes through provider customer didn't authorise | Provider allowlist per-customer; type-level check on `Provider::send()` against allowlist |

---

## UX Pitfalls

(M1 is foundation; UX is downstream of validated install. Limited list.)

| Pitfall | User Impact | Better Approach |
|---------|-------------|-----------------|
| `osagent --version` returns just semver | Operator can't verify which feature-set is deployed | Print semver + MANIFEST hash + binary SHA256 + zeroclaw upstream SHA |
| MANIFEST diff failure on missing optional feature | Operator confused by "wizard doesn't have `local-llm-foo`" when they didn't ask for it | `manifest --diff` distinguishes "config asks for missing feature" (block install) from "binary has feature config didn't reference" (warn, log to audit) |
| `local-only` policy refuses to serve, message is opaque | Operator thinks engineer is broken | Refusal message names the policy + the unreachable oracle + the path to re-enable: "local-only policy active; oracle at ... unreachable; refusing to route to cloud. Bring oracle back or run `osagent config set provider_policy local-first` to allow fallback." |

---

## "Looks Done But Isn't" Checklist

- [ ] **Wizard MCP-free:** `nm | grep mcp` empty — also verify with `cargo bloat --crates`, `strings | grep mcp`, source-grep. All four layers.
- [ ] **MANIFEST.toml accurate:** `[declared]` and `[detected]` sections agree; hash anchored to witness.
- [ ] **Feature-unification audited:** `cargo tree --format '{p} {f}'` for both bins shows expected feature sets only.
- [ ] **`--check-cfg` enabled:** `cargo check --workspace --all-targets` runs without `unexpected_cfgs` warnings.
- [ ] **resolver = "2" pinned:** workspace Cargo.toml line present.
- [ ] **All deps `default-features = false`:** grep returns expected list; no implicit defaults leak in.
- [ ] **`dep:` prefix on all optional deps:** RFC 3491 migration done.
- [ ] **Clean-VM CI test green:** install task runs against fresh `ubuntu:24.04` with no pre-state.
- [ ] **Reproducible build:** two CI runs of same commit produce byte-identical binary.
- [ ] **AMQP TLS bootstrap shared:** one `osagent-amqp` crate; no per-service re-implementation.
- [ ] **Cert mirror to `$HOME`:** ansible task mirrors `engineer-amqp` certs into `/home/engineer/.zeroclaw/certs/`.
- [ ] **Audit log pre-touch:** ansible `file: state=touch owner=engineer mode=0640 modification_time=preserve` ahead of service start.
- [ ] **StartLimitIntervalSec on engineer/wizard units:** in `[Unit]` block, not `[Service]`.
- [ ] **Bridge has no `chown` syscall:** source-grep + cap-check.
- [ ] **No `amqp://` (plaintext):** source-grep + strings-grep on binary.
- [ ] **NOTICE auto-generated from cargo-about:** committed alongside binary, signed.
- [ ] **`cargo deny check licenses` passes:** no copyleft surprises.
- [ ] **Telemetry strip:** `cargo tree | grep -Ei 'sentry|posthog'` empty; network-namespace CI test passes.
- [ ] **Sandbox-aware paths:** all engineer/wizard runtime reads from `$HOME/`, not `/opt/sovereign-shield/`.
- [ ] **OS-MDashboard `/ws/chat` endpoint preserved:** integration test against chat-relay returns 200.
- [ ] **UPSTREAM_SYNC.md runbook present:** committed under `sovereign-shield-backup/documentation/osAgent/`.
- [ ] **Subtree merge configured, not submodule:** `.git/config` has subtree remote, no `.gitmodules` for zeroclaw.

---

## Recovery Strategies

| Pitfall | Recovery Cost | Recovery Steps |
|---------|---------------|----------------|
| MCP leaked into wizard, detected post-deploy | HIGH | (1) Disable wizard binary at install-guide via `state: stopped enabled: false` (class #15 pattern); (2) hot-fix Cargo.toml + crate split; (3) emergency CI gate verification including 4 layers; (4) re-sign artifact; (5) re-deploy. Audit log MUST capture every wizard tool call between leak-detection and re-deploy for ex-post review. |
| MANIFEST.toml lied (binary has more than manifest claims) | HIGH | Same as MCP leak; investigate root cause (likely feature unification per Pitfall 1) before re-deploy. |
| Quarterly merge conflict catastrophic | MEDIUM | (1) Abort merge; (2) inspect `UPSTREAM_SYNC.md` log for accumulated divergence shape; (3) refactor OUR changes into osAgent-only crates that wrap upstream cleanly; (4) re-attempt merge from cleaner baseline. May skip a quarter — acceptable if next quarter is recoverable. |
| Pin-drift (Class #10, Pitfall 4) | LOW | `/usr/local/bin/osagent-engineer --version` reveals SHA mismatch; restore from signed artifact; investigate which step bypassed the SHA256 gate. |
| Live-vs-repo drift (Pitfall 5) | LOW | `ansible-playbook bootstrap_host.yml --tags osagent` re-applies; assert `osagent --version` matches expected post-deploy. |
| Class #11 / Pitfall 11 workbench ownership contamination | LOW | `chown-enforcer.timer` auto-heals within 5 min; manual run `sudo /usr/local/sbin/chown-enforcer.sh` immediate. |
| Class #12 / Pitfall 12 cross-UID drift | LOW | Same as #11, plus add the chown-back to the offending script. |
| Class #13 / Pitfall 13 daemon-restart catastrophic | HIGH | `docker compose -p <project> up -d --remove-orphans` per compose project; assert no `exited` containers; Phase 3 EXIT smoke gate. |
| Class #18 / Pitfall 14 TLS pattern missing on new client | MEDIUM | Lift to shared helper, propagate to siblings; test each affected service. |
| Class #21 / Pitfall 15 TLS race after broker restart | LOW | Retry/until block in ansible; bridge retry budget swallows it on next attempt. |
| Class #22 / Pitfall 16 audit log not pre-created | LOW | `file: state=touch` ansible task; restart daemon; tail journal. |
| Provider local-only fell back to cloud (Pitfall 24) | HIGH | Audit log must show the policy-violation; refund customer's privacy SLA (if applicable); hot-fix type-level separation; verify no other provider chain has the same hole. |
| Non-deterministic DCE different on customer hardware (Pitfall 19) | HIGH | Customer's binary is suspect; rebuild on self-hosted signed runner; deploy signed binary, drop the customer-built one. |

---

## Pitfall-to-Phase Mapping

| Pitfall | Prevention Phase | Verification |
|---------|------------------|--------------|
| 1 — Feature unification leak | 1.4 (Strip dead features) | 4-layer MCP gate green in CI; `cargo tree` shows expected feature sets |
| 2 — `#[cfg]` typos silently always-false | 1.4 | `cargo check` no `unexpected_cfgs` warnings |
| 3 — `nm` passes but code inlined | 1.4 + 1.3 (Strip dead crates baseline) | `cargo bloat` shows zero size for stripped crates; reproducibility CI |
| 4 — Upstream pin-drift (class #10) | 1.1 (Fork+attribution+sync runbook) + 1.6 (Install task) | `get_url` + `checksum: sha256:` in install task; `--version` asserts hash |
| 5 — Live-vs-repo drift (class #7) | 1.6 | `lint_script_drift.sh` extended to osAgent paths |
| 6 — Wizard inherits engineer surface via shared crate | 1.2 (Workspace restructure) | `cargo tree -i <engineer-only-crate>` shows no wizard reverse-dep |
| 7 — Quarterly merge degenerates | 1.1 | UPSTREAM_SYNC.md runbook + diff-stat budget + CI suite |
| 8 — MANIFEST.toml lies | 1.5 (Telemetry audit + MANIFEST) | `[declared]` vs `[detected]` agree; hash anchored |
| 9 — EnvironmentFile change no restart (class #2) | 1.6 | ansible handler chain |
| 10 — Invisible-on-rerun (class #5) | 1.6 | Clean-VM CI test |
| 11 — Workbench ownership (class #11) | 1.2 (bridge tool has no chown) | source-grep CI lint + cap-check |
| 12 — Cross-UID writer drift (class #12) | 1.6 (if config-refresh added) | chown-back step + lint |
| 13 — Daemon restart without recovery (class #13) | 1.6 | "no dockerd restart" constraint documented |
| 14 — TLS/sibling-pattern (class #18) | 1.2 | shared `osagent-amqp` crate; `grep amqp://` empty |
| 15 — TLS listener race (class #21) | 1.2 + 1.6 | retry budget in `osagent-amqp`; smoke probe at install end |
| 16 — Audit log pre-create (class #22) | 1.6 | ansible `file: state=touch` ahead of service start |
| 17 — Implicit features deprecation (RFC 3491) | 1.4 | `dep:` prefix audit; CI lint |
| 18 — Resolver version mid-fork shift | 1.2 | `resolver = "2"` pinned; CI assertion |
| 19 — Non-deterministic DCE | 1.5 | Reproducibility CI; sign at self-hosted runner |
| 20 — Telemetry strip incomplete | 1.5 | `cargo tree` audit, network-namespace CI test, egress whitelist |
| 21 — Embedded credentials (class #16) | 1.6 | install-task lint; secrets in Vault not files |
| 22 — Workbench-vs-life-config (class #17) | 1.2 | engineer tool list excludes life-config-write tools |
| 23 — License attribution drift | 1.1 | `cargo about generate` in CI; `cargo deny check licenses` |
| 24 — Provider-policy `local-only` fallback (M2 forward-looking) | Out of M1; document constraint in 1.2 | type-level separation specified in 1.2 architecture; verified in M2 |

---

## Sources

### Cargo / Rust ecosystem (2025-2026)

- [Cargo Workspace and the Feature Unification Pitfall — nickb.dev](https://nickb.dev/blog/cargo-workspace-and-the-feature-unification-pitfall/) — primary reference for Pitfall 1.
- [Features — The Cargo Book](https://doc.rust-lang.org/cargo/reference/features.html) — feature semantics.
- [RFC 2957 — cargo features2](https://rust-lang.github.io/rfcs/2957-cargo-features2.html) — resolver v2 behavior.
- [RFC 3491 — remove implicit features](https://rust-lang.github.io/rfcs/3491-remove-implicit-features.html) — Pitfall 17.
- [RFC 3143 — cargo weak-namespaced-features](https://rust-lang.github.io/rfcs/3143-cargo-weak-namespaced-features.html) — `dep:` prefix semantics.
- [Cargo issue #14774 — Tracking workspace feature-unification](https://github.com/rust-lang/cargo/issues/14774) — Pitfall 18 (resolver shifts).
- [Cargo issue #11329 — workspaces.dependencies causes ignore of default-features = false](https://github.com/rust-lang/cargo/issues/11329) — Pitfall 1.
- [Cargo issue #8366 — default-features = false not working for dependency inside workspace](https://github.com/rust-lang/cargo/issues/8366) — Pitfall 1.
- [Cargo issue #1886 — --no-default-features is not applied to dependencies](https://github.com/rust-lang/cargo/issues/1886) — Pitfall 1.
- [Effective Rust Item 26 — Be wary of feature creep](https://effective-rust.com/features.html) — fork-discipline lessons.
- [Rust issue #150462 — Non-deterministic dead-code elimination](https://github.com/rust-lang/rust/issues/150462) — Pitfall 19.
- [RFC 3013 — conditional-compilation-checking (`--check-cfg`)](https://rust-lang.github.io/rfcs/3013-conditional-compilation-checking.html) — Pitfall 2.
- [Rust 1.85.0 + Rust 2024 release announcement](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/) — edition transition (Pitfall — Rust 2024 in answers section).
- [codeandbitters — Updating a large codebase to Rust 2024 edition](https://codeandbitters.com/rust-2024-upgrade/) — migration pitfalls report.
- [rustc Codegen Options — `-C strip`](https://doc.rust-lang.org/rustc/codegen-options/index.html) — Pitfall 3 (symbol stripping).
- [Cargo Profiles — `[profile.release]` lto, codegen-units, strip](https://doc.rust-lang.org/cargo/reference/profiles.html) — Pitfall 3 + 19.
- [rustc-dev-guide monomorph collector](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_monomorphize/collector/index.html) — Pitfall 6 (trait-object monomorphization use-edges).
- [inventory crate / cast_trait_object_macros notes](https://crates.io/crates/cast_trait_object_macros) — Pitfall 6 (registry crates).
- [GitHub Docs — About Git subtree merges](https://docs.github.com/en/get-started/using-git/about-git-subtree-merges) — Pitfall 7 (subtree vs submodule).
- [rustc-dev-guide — Using external repositories](https://rustc-dev-guide.rust-lang.org/external-repos.html) — Pitfall 7 (subtree practice in the Rust project itself).
- [How to Override Dependencies with `[patch]` — RustFAQ](https://www.rustfaq.org/en/how-to-override-dependencies-with-patch-in-cargo/) — fork-management with `[patch]`.

### sovereign-shield install-guide invariants (carry-forward classes)

- `d:/Repositories/sovereign-shield-install-guide/CLAUDE.md` — anti-pattern classes #2, #5, #7, #10, #11, #12, #13, #15, #16, #17, #18, #21, #22 (referenced throughout).
- Prior-session learning: 2026-04-22 gateway port collision (class #8 pattern).
- Prior-session learning: 2026-04-22 v0.7.3 pin-drift incident → direct tarball + SHA256.
- Prior-session learning: 2026-04-23 workbench-ownership incident → class #11 three-layer defense.
- Prior-session learning: 2026-04-24 persist-service cross-UID ownership drift → class #12 chown-back pattern.
- Prior-session learning: 2026-04-27 dockerd-restart-without-live-restore → class #13.
- Prior-session learning: 2026-04-28 vault audit-log permission denied → class #22.
- Prior-session learning: 2026-04-28 PAT-in-workbench-`.git/config` → classes #16 + #17.
- Prior-session learning: 2026-04-28 chameleon pika TLS missing → class #18.

### Project context

- `d:/Repositories/osAgent/.planning/PROJECT.md` — 42 ratified architectural decisions; constraints; out-of-scope list. All decisions referenced inline by number.

### Confidence

- **HIGH** — Pitfalls 1-5, 7-22 (Cargo invariants are official-doc-anchored; install-guide carry-forwards are lived experience).
- **MEDIUM** — Pitfalls 6, 19 (trait-object monomorphization leak path is mechanically clear from rustc-dev-guide but observed-in-the-wild for our exact zeroclaw codebase needs verification once workspace structure is concrete in 1.2).
- **MEDIUM** — Pitfall 23 (license auto-aggregation tools exist; mechanics of NOTICE-aggregation under MIT + Apache dual-license forks are documented but project-specific edge cases possible).
- **MEDIUM/LOW** — Pitfall 24 (provider-policy specifics depend on the M2 provider crate design which is forward-looking).

---
*Pitfalls research for: osAgent — tailored fork of zeroclaw v0.7.5 for sovereign-shield platform deployment.*
*Researched: 2026-06-12*
