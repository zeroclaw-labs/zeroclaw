# Cargo Audit / Deny Policy

This document explains the relationship between `.cargo/audit.toml` and
`deny.toml`, the rationale for every ignored advisory, and the workflow
for adding or removing entries. It is the maintainer-facing companion
to the in-file comments.

**Audience:** maintainers triaging `cargo audit` and `cargo deny` CI failures,
or contributors opening a PR that bumps a dependency and needs to drop a
no-longer-needed ignore.

---

## Two tools, two lockfiles

`cargo audit` and `cargo deny check advisories` look at the same
`Cargo.lock` but differ in scope:

- **`cargo audit` (`.cargo/audit.toml`)** reads the entire lockfile and
  reports every RustSec advisory touching any package, including
  transitive dependencies outside the workspace's dep tree.
- **`cargo deny` (`deny.toml`)** is graph-aware: it walks the actual
  resolved dep graph and only reports advisories for crates actually
  pulled in by the workspace.

The result is that `cargo audit` can fail with advisories
`cargo deny` considers non-applicable, even when both files are
configured against the same `Cargo.lock`. The drift between the two
tools is tracked in **#8519**.

When the two tools disagree, the Security job in
`.github/workflows/ci.yml` runs **both** `cargo audit` and
`cargo deny check advisories` as hard gates. A non-zero exit from
either tool blocks the PR.

The difference between the tools is **scope**, not enforcement:
`cargo audit` reports every advisory touching the lockfile, while
`cargo deny` only reports advisories for crates in the resolved
workspace graph. Use the narrower `cargo deny` result to confirm an
advisory is not actually pulled in, but treat both CI failures as
blocking.

---

## Ignore categories

There are two kinds of ignored advisory:

### 1. Real CVE / vulnerability (must be remediated)

These ignores mark advisories with an exploitable bug. They are
**temporary** and must be removed when a fix lands. The remaining live
example is the wasmtime-wasi CVE bundle in **#8519**:
`RUSTSEC-2026-0149`, `-0182`, `-0188`, which is cleared by the
wasmtime `43` → `45.0.3` bump in `crates/zeroclaw-plugins/Cargo.toml`
(see PR #8542, awaiting maintainer re-approval after the latest
`upstream/master` merge).

**Process for this category:**

- Add the entry with a single-line `reason` ending in the tracking
  issue URL or PR number.
- When a fix lands, remove the entry from **both** `.cargo/audit.toml`
  *and* `deny.toml` in the same PR. A drift here re-introduces the
  original CI failure.
- Each file has a one-line `── tracking #... ──` header above its
  block. Preserve the header when adding entries to the same category;
  introduce a new header for a new category.

### 2. Unmaintained-crate advisory (no fix available)

These advisories are informational. The crate has no maintained
successor on the dependency lines we use. They are
**semi-permanent**; the entry stays until the underlying dependency
is replaced (e.g. GTK3 → GTK4, rumqttc upgrade that pulls
`rustls-webpki 0.103.x`).

Live, deny+audit (both files):

- **`rustls-pemfile` (`RUSTSEC-2025-0134`)**: unmaintained;
  transitive dep awaiting upstream migration to `rustls-pki-types`.
  Present in both `deny.toml` and `audit.toml`.

Live, audit-only (`cargo deny`'s resolved graph no longer pulls these
in, but they remain in `Cargo.lock` and `cargo audit` reads the whole
lockfile — remove from `audit.toml` only once the crate is dropped
from `Cargo.lock` entirely, e.g. via `cargo update` or a dependency
bump):

- **`rustls-webpki` (4 entries, `RUSTSEC-2026-0049`, `-0098`, `-0099`,
  `-0104`)**: 0.102.x copy is in `Cargo.lock` but not in the resolved
  dependency graph. `cargo deny` does not flag it; `cargo audit` does.
- **GTK3 stack (11 entries, `RUSTSEC-2024-0411..-0420`, `-0429`)**:
  `gdk`/`gtk`/`atk`-family gtk-rs bindings and `glib`. Present in
  `Cargo.lock` — `zeroclaw-desktop` (Tauri) was removed in PR #8544
  and reintroduced in PR #8565 — but not needed by `cargo deny`'s
  current default-target resolved graph (`cargo deny check bans` and
  `check advisories` both pass clean without these ignores). Do not
  assume this means the crates are gone from the tree; re-check with
  `grep '^name = "<crate>"$' Cargo.lock` before removing from
  `audit.toml`. Tracking #8519.
- **`unic-*` (5 entries, `RUSTSEC-2025-0075`, `-0080`, `-0081`,
  `-0098`, `-0100`)**: Unicode data tables, previously transitive via
  `pulldown-cmark` and `mime_guess`. Same drift as above; tracking
  #8519.
- **macro / font helpers (2 entries, `RUSTSEC-2026-0173`,
  `-2024-0388`)**: `proc-macro-error2`, `derivative`. Same drift;
  tracking #8519.
- **`bincode` (`RUSTSEC-2025-0141`)**: previously transitive via
  `probe-rs builtin-targets`. Same drift; tracking #8519.
- **`instant` (`RUSTSEC-2024-0384`)**: informational-only unmaintained
  advisory. Same drift; tracking #8519.

Resolved (no longer in `Cargo.lock` at all — safe to drop from both
files, or already dropped):

- **`rand` (`RUSTSEC-2026-0097`)**: re-entrancy unsoundness; the
  0.8.x copy affected by this advisory is no longer resolved by any
  workspace crate.

**Process for this category:**

- Use a short reason naming the crate role, e.g.
  `gtk-rs GTK3 bindings; transitive via zeroclaw-desktop/tauri`.
- Do not add `; tracking #...` for entries that are stable
  unmaintained warnings and unlikely to be resolved in the next
  release cycle.
- An entry drops out of `deny.toml` as soon as `cargo deny`'s resolved
  graph no longer needs it — that is a graph fact, not a lockfile fact,
  and it can change on the next dependency bump or feature change
  without the crate leaving `Cargo.lock`. It only drops out of
  `audit.toml` once the crate is gone from `Cargo.lock` entirely.
  Removing an audit-only entry while the crate is still resolvable
  reintroduces the CI failure this doc exists to prevent — always
  check `Cargo.lock` directly, not just `cargo deny`'s last result.

---

## Tracking issues

- **#8519**: *Reconcile cargo-audit ignores and remediate wasmtime-wasi
  CVEs.* Master issue for the audit/deny drift. The GTK3 stack, unic-*,
  macro/font helpers, `bincode`, and `instant` are no longer needed in
  `deny.toml` (removed from the resolved dependency graph) but remain
  audit-only ignores in `.cargo/audit.toml` until they're gone from
  `Cargo.lock`. `rand` is fully resolved and removed from both files.
  Remaining deny+audit live ignore: `rustls-pemfile`. Remaining
  audit-only ignores: `rustls-webpki` (4) plus the 20 lockfile-stale
  entries above.
- **#8059**: *Policy cleanup: deny.toml ignored-advisory tracking,
  multiple-versions, wildcards.* piiiico's RFC on adding per-entry
  rationale to `deny.toml` ignore blocks. This doc is the
  higher-level policy view; the in-file comments are the per-entry
  tracking.

---

## Local validation

Run before pushing any PR that touches `.cargo/audit.toml` or
`deny.toml`:

```bash
cargo install cargo-audit --locked    # one-time
cargo audit                          # binds the CI gate
cargo deny check advisories          # graph-aware cross-check
cargo fmt --all -- --check
```

If `cargo audit` reports an advisory that is not on the ignore list,
either add it (with rationale and tracking issue) or fix the
underlying dep; there is no third option.

If `cargo deny` reports an advisory that `cargo audit` does not, the
two tools have drifted again. Open or update the tracking issue.

---

## Change log

- 2026-07-19: Corrected the 07-06 pass, which removed 20 entries from
  `.cargo/audit.toml` (`unic-*`, `proc-macro-error2`, `derivative`,
  `instant`, `bincode`, `glib`, all 10 GTK3 stack entries) that were
  still present in `Cargo.lock` and still reported by `cargo audit`.
  Restored those 20 as audit-only ignores; they remain removed from
  `deny.toml`, where `cargo deny`'s resolved graph still doesn't need
  them even after the `zeroclaw-desktop` (Tauri) reintroduction in PR
  #8565 (`cargo deny check advisories`/`bans` verified clean). `rand`
  (`RUSTSEC-2026-0097`) is confirmed fully out of `Cargo.lock` and
  stays removed from both files.
- 2026-07-06: Removed advisory ignores from `deny.toml` for crates no
  longer in `cargo deny`'s resolved dependency graph: `unic-*`
  (5 entries), `proc-macro-error2`, `derivative`, `instant`, `bincode`,
  `glib`, all GTK3 stack entries, `rand` (0.8.x no longer resolved),
  and the `rustls-webpki` entries (0.102.x no longer in resolved
  graph). Remaining deny+audit ignore: `rustls-pemfile` (1). Remaining
  audit-only ignores: `rustls-webpki` (4 entries; in `Cargo.lock` but
  not in resolved dep graph). Closes the Security CI gate failure from
  stale `advisory-not-detected` warnings in `cargo deny`.
- 2026-07-01: Updated after `upstream/master` merge. Documented that
  the GTK3 stack was resolved by PR #8544 (Tauri desktop removal),
  `proc-macro-error` ignore was dropped, `ttf-parser` is being handled
  by PR #8547, and the `unic-*` group remains blocked by upstream
  `pulldown-cmark` / `mime_guess`. (PR #8543)
- 2026-06-30: Initial doc. Created alongside PR #8542 (wasmtime
  43 → 45.0.3 bump) and PR #8519 (the master audit-tracking issue).
