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

Live groups:

- **`rustls-pemfile` (`RUSTSEC-2025-0134`)**: unmaintained;
  transitive dep awaiting upstream migration to `rustls-pki-types`.
  Present in both `deny.toml` and `audit.toml`.

- **`rustls-webpki` (4 entries, `RUSTSEC-2026-0049`, `-0098`, `-0099`,
  `-0104`)**: 0.102.x copy is in `Cargo.lock` but not in the resolved
  dependency graph (CI Rust 1.93.0). `cargo deny` does not flag it;
  `cargo audit` does (reads full lockfile). Ignore entries are
  **audit.toml only**.

Resolved groups:

- **GTK3 stack (10 entries, `RUSTSEC-2024-0411..-0420`)**: pulled in
  transitively by the now-removed `zeroclaw-desktop` (Tauri →
  webkit2gtk → gtk-rs bindings). These ignore entries were dropped in
  PR #8544 along with the desktop app. No GTK3 code remains in the
  workspace.
- **`unic-*` (5 entries, `RUSTSEC-2025-0075`, `-0080`, `-0081`,
  `-0098`, `-0100`)**: Unicode data tables previously transitive via
  `pulldown-cmark` and `mime_guess`. No longer in the dependency tree.
- **macro / font helpers (3 entries, `RUSTSEC-2026-0173`,
  `-2024-0388`, `-2024-0384`)**: `proc-macro-error2`, `derivative`,
  `instant`. No longer in the dependency tree.
- **`bincode` (`RUSTSEC-2025-0141`)**: previously transitive via
  `probe-rs builtin-targets`. No longer in the dependency tree.
- **`glib` (`RUSTSEC-2024-0429`)**: previously transitive via
  `zeroclaw-desktop/tauri/webkit2gtk`. No longer in the dependency
  tree.
- **`rand` (`RUSTSEC-2026-0097`)**: re-entrancy unsoundness; no
  longer in the resolved dependency graph (CI Rust 1.93.0 resolves
  only rand 0.9.x+).
- **`rustls-webpki` (4 entries, `RUSTSEC-2026-0049`, `-0098`, `-0099`,
  `-0104`)**: 0.102.x copy no longer in the resolved dependency graph;
  only the patched 0.103.x copy remains.

**Process for this category:**

- Use a short reason naming the crate role, e.g.
  `gtk-rs GTK3 bindings; transitive via zeroclaw-desktop/tauri/webkit2gtk`.
- Do not add `; tracking #...` for entries that are stable
  unmaintained warnings and unlikely to be resolved in the next
  release cycle.
- When a replacement lands upstream and the dep gets bumped, remove
  the entry from both files.

---

## Tracking issues

- **#8519**: *Reconcile cargo-audit ignores and remediate wasmtime-wasi
  CVEs.* Master issue for the audit/deny drift. The GTK3, unic-*,
  macro/font helper, bincode, glib, and rand ignore entries have been
  removed (no longer in the dependency tree). Remaining deny+audit live
  ignore: `rustls-pemfile`. Remaining audit-only ignores: `rustls-webpki`
  (4 entries; 0.102.x in Cargo.lock but not in resolved dep graph).
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

- 2026-07-06: Removed stale advisory ignores for crates no longer in the
  CI-resolved dependency tree (Rust 1.93.0): `unic-*` (5 entries),
  `proc-macro-error2`, `derivative`, `instant`, `bincode`, `glib`, all
  GTK3 stack entries, `rand` (0.8.x no longer resolved), and
  `rustls-webpki` entries from `deny.toml` (0.102.x no longer in
  resolved graph). Remaining deny+audit ignore: `rustls-pemfile` (1).
  Remaining audit-only ignores: `rustls-webpki` (4 entries; in
  `Cargo.lock` but not in resolved dep graph). Closes the Security CI
  gate failure from stale `advisory-not-detected` warnings.
- 2026-07-01: Updated after `upstream/master` merge. Documented that
  the GTK3 stack was resolved by PR #8544 (Tauri desktop removal),
  `proc-macro-error` ignore was dropped, `ttf-parser` is being handled
  by PR #8547, and the `unic-*` group remains blocked by upstream
  `pulldown-cmark` / `mime_guess`. (PR #8543)
- 2026-06-30: Initial doc. Created alongside PR #8542 (wasmtime
  43 → 45.0.3 bump) and PR #8519 (the master audit-tracking issue).
