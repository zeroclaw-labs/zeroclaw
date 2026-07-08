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

- **`unic-*` (5 entries, `RUSTSEC-2025-0075`, `-0080`, `-0081`,
  `-0098`, `-0100`)**: Unicode data tables. Transitive via
  `pulldown-cmark` and `mime_guess`. Both crates still depend on
  `unicase` in their latest releases; replacing either requires
  rewriting downstream code (`apps/zerocode/src/chat.rs` for
  `pulldown-cmark`, multiple MIME-type call sites for `mime_guess`) or
  waiting for upstream releases that drop the dependency. Tracking
  #8519.
- **macro / font helpers (3 entries, `RUSTSEC-2026-0173`,
  `-2024-0388`, `-2026-0192`)**: `proc-macro-error2`, `derivative`,
  `ttf-parser`. Transitive derive / macro helpers; replacing each
  requires coordinated upstream migration.
  - `proc-macro-error` (`RUSTSEC-2024-0370`) was cleared when
    `zeroclaw-desktop` (Tauri) was removed in PR #8544.
  - `ttf-parser` is handled by PR #8547, which removes the `rag-pdf`
    feature and the `pdf-extract -> lopdf -> ttf-parser` chain.
  Tracking #8519.

Resolved groups:

- **GTK3 stack (10 entries, `RUSTSEC-2024-0411..-0420`)**: pulled in
  transitively by the now-removed `zeroclaw-desktop` (Tauri →
  webkit2gtk → gtk-rs bindings). These ignore entries were dropped in
  PR #8544 along with the desktop app. No GTK3 code remains in the
  workspace.

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
  CVEs.* Master issue for the audit/deny drift and the unmaintained
  GTK3 / unic-* / macro entries. Updates belong in the comments of
  the affected entries, not in this file.
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

- 2026-07-01: Updated after `upstream/master` merge. Documented that
  the GTK3 stack was resolved by PR #8544 (Tauri desktop removal),
  `proc-macro-error` ignore was dropped, `ttf-parser` is being handled
  by PR #8547, and the `unic-*` group remains blocked by upstream
  `pulldown-cmark` / `mime_guess`. (PR #8543)
- 2026-06-30: Initial doc. Created alongside PR #8542 (wasmtime
  43 → 45.0.3 bump) and PR #8519 (the master audit-tracking issue).
