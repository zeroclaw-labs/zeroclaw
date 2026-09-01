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

The result is that `cargo audit` can report advisories
`cargo deny` considers non-applicable, even when both files are
configured against the same `Cargo.lock`. The drift between the two
tools is tracked in **#8519**.

The Security job in `.github/workflows/ci.yml` runs **both** `cargo
audit` and `cargo deny check advisories`. A non-zero exit from either
tool blocks the PR. What actually fails each tool differs by category:

- **`cargo audit`** (bare, no `--deny warnings`): vulnerability
  advisories are errors (exit 1); informational and unmaintained
  advisories are reported as allowed warnings and exit 0.
- **`cargo deny check advisories`**: vulnerability *and* unmaintained
  advisories for crates in the resolved graph are errors (exit 1) —
  that is exactly why the three live unmaintained denies
  (`rustls-pemfile`, `proc-macro-error2`, `bitmaps`) must stay in
  `deny.toml`.
  A stale graph-ignore instead emits `advisory-not-detected`, which is
  a warning (exit 0); it is never triggered by removing an entry from
  `.cargo/audit.toml`.

An audit-only ignore covers a crate `cargo deny`'s resolved graph does
not pull in, so it affects only `cargo audit`: removing it while the
crate stays locked re-reports the advisory as an allowed warning (exit
0), not a CI failure. Keeping it is therefore accepted full-lock noise
control, not a hard-gate bypass — but dropping it does not break the
gate.

The difference between the tools is **scope**, not severity:
`cargo audit` reports every advisory touching the lockfile, while
`cargo deny` only reports advisories for crates in the resolved
workspace graph. Use the narrower `cargo deny` result to confirm an
advisory is not actually pulled in; keep the audit-only entry while
the crate remains locked.

---

## Ignore categories

There are two kinds of ignored advisory:

### 1. Real CVE / vulnerability (must be remediated)

These ignores mark advisories with an exploitable bug. They are
**temporary** and must be removed when a fix lands. There is currently
no live entry in this category: the wasmtime-wasi CVE bundle tracked in
**#8519** (`RUSTSEC-2026-0149`, `-0182`, `-0188`, then `-0222`) was
cleared by the `45.0.3` bump in PR #8542 and the subsequent `47.0.3`
bump in PR #9589, which also removed the temporary waivers from both
files.

**Process for this category:**

- Add the entry with a single-line `reason` ending in the tracking
  issue URL or PR number.
- When a fix lands, remove the entry from **both** `.cargo/audit.toml`
  *and* `deny.toml` in the same PR. Leaving a stale ignore behind makes
  `cargo deny` emit `advisory-not-detected` (a warning, not a gate
  failure) for the entry, so keep the two files in sync.
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
- **`proc-macro-error2` (`RUSTSEC-2026-0173`)**: unmaintained
  derive/attribute macro helper. Still in `cargo deny`'s resolved graph
  via `matrix-sdk` dev-deps (`aquamarine`) in `zeroclaw-channels`, so it
  needs the ignore in both files.
- **`bitmaps` (`RUSTSEC-2026-0247`)**: unmaintained; all versions are
  affected and no patched version is available. Locked `matrix-sdk`
  reaches `imbl -> bitmaps` both directly and through `eyeball-im`.
  Remove the `deny.toml` entry only after `cargo deny` no longer resolves
  an affected `bitmaps`; remove the `.cargo/audit.toml` entry only after
  no affected `bitmaps` remains in `Cargo.lock`. Tracking #9899 and
  matrix-org/matrix-rust-sdk#6859.

The locked `bitmaps 3.2.1` also matches the separate informational
unsoundness advisory `RUSTSEC-2025-0167`, which describes memory-corruption
risk and has no patched release. The `RUSTSEC-2026-0247` waiver does not
ignore that advisory. Under the repository's current Security-job commands,
it remains an allowed `cargo audit` warning rather than a denied advisory.

Live, audit-only (`cargo deny`'s resolved graph no longer pulls these
in, but they remain in `Cargo.lock` and `cargo audit` reads the whole
lockfile — remove from `audit.toml` only once the crate is either
dropped from `Cargo.lock` entirely, e.g. via `cargo update` or a
dependency bump, or every locked version of it is patched/unaffected
per the advisory):

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
- **macro / font helpers (1 entry, `RUSTSEC-2024-0388`)**:
  `derivative`. Same drift; tracking #8519.
- **`bincode` (`RUSTSEC-2025-0141`)**: previously transitive via
  `probe-rs builtin-targets`. Same drift; tracking #8519.
- **`instant` (`RUSTSEC-2024-0384`)**: informational-only unmaintained
  advisory. Same drift; tracking #8519.

Resolved (safe to drop from both files — either the crate is gone
from `Cargo.lock` entirely, or every locked version is patched /
unaffected by the advisory):

- **`rand` (`RUSTSEC-2026-0097`)**: re-entrancy unsoundness in a
  custom global logger. `Cargo.lock` still resolves `rand` 0.8.6,
  0.9.4, and 0.10.1 — the crate is not absent — but the
  [advisory](https://rustsec.org/advisories/RUSTSEC-2026-0097.html)
  marks all three of those versions as patched, so no locked copy is
  affected and the ignore is no longer needed.

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
  `audit.toml` once the crate is either gone from `Cargo.lock`
  entirely or every locked version of it is patched/unaffected per the
  advisory. Removing an audit-only entry while a still-affected
  version remains resolvable makes `cargo audit` report the advisory
  again as an allowed warning (it does not fail CI — `cargo audit`
  exits 0 on warnings under the configured invocation) — but it drops
  the accepted full-lock noise control this document records. Always
  check `Cargo.lock` and the advisory's patched-version range directly,
  not just `cargo deny`'s last result.

---

## Tracking issues

- **#8519**: *Reconcile cargo-audit ignores and remediate wasmtime-wasi
  CVEs.* Master issue for the audit/deny drift. The wasmtime-wasi CVE
  bundle is fully remediated (PR #8542, then PR #9589) and no longer
  needs an ignore in either file. The GTK3 stack, unic-*, macro/font
  helpers, `bincode`, and `instant` are no longer needed in
  `deny.toml` (removed from the resolved dependency graph) but remain
  audit-only ignores in `.cargo/audit.toml` until they're gone from
  `Cargo.lock`. `rand` is removed from both files because every
  locked version (0.8.6, 0.9.4, 0.10.1) is patched per the advisory,
  not because the crate left `Cargo.lock`. Remaining deny+audit live
  ignores: `rustls-pemfile`, `proc-macro-error2`, `bitmaps`. Remaining
  audit-only ignores: `rustls-webpki` (4) plus the 19 lockfile-stale
  entries above.
- **#9899**: *Triage and remove bitmaps unmaintained advisory waiver.*
  Tracks the `RUSTSEC-2026-0247` waiver described above and owns
  acceptance and revisit of the visible `RUSTSEC-2025-0167` warning.
  Re-evaluate both when Matrix SDK dependencies change; stop accepting
  `RUSTSEC-2025-0167` once no affected `bitmaps` remains in `Cargo.lock`
  or the advisory marks every locked version patched/unaffected. The
  `RUSTSEC-2026-0247` waiver does not suppress that separate warning.
  Upstream replacement work is tracked in matrix-org/matrix-rust-sdk#6859.
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

If `cargo audit` reports an error-class advisory that is not on the ignore
list, either add a temporary ignore with rationale and tracking or fix the
underlying dependency. An informational advisory may remain an unignored
warning only when its acceptance, owner, and revisit/removal condition are
documented; `RUSTSEC-2025-0167` is intentionally visible under that rule.

If `cargo deny` reports an advisory that `cargo audit` does not, the
two tools have drifted again. Open or update the tracking issue.

---

## Change log

- 2026-08-11: Mirrored the exact `RUSTSEC-2026-0247` `bitmaps` waiver
  from `deny.toml` into `.cargo/audit.toml` and added its dependency
  routes, #9899 lifecycle, and tool-specific removal conditions to this
  inventory. Documented that `RUSTSEC-2025-0167` is a separate allowed
  warning, not covered by the unmaintained-advisory waiver.
- 2026-08-04: Rebased onto `upstream/master`, which merged PR #9589
  (wasmtime `45.0.3` → `47.0.3`, clearing `RUSTSEC-2026-0222` and
  removing the waiver from both files). Removed the now-stale
  wasmtime references from the "Real CVE" category, the "Live,
  audit-only" list, and the #8519 tracking summary above — there is
  currently no live entry in the real-CVE category.
- 2026-08-03: Corrected the in-file block classification in
  `.cargo/audit.toml` to match the actual file contents: `wasmtime`
  (`RUSTSEC-2026-0222`) is back in the audit-only block (it was
  removed from `deny.toml`, so "Live in both" was wrong), and
  `proc-macro-error2` (`RUSTSEC-2026-0173`) moved to the "Live in
  both" block (it is present in `deny.toml`). Also corrected the
  enforcement claims per tool: under the configured invocations,
  informational/unmaintained advisories are allowed warnings in
  `cargo audit` (exit 0) but unmaintained advisories for crates in
  `cargo deny`'s resolved graph are errors (exit 1) — hence the two
  live unmaintained denies — while `advisory-not-detected` is `cargo
  deny`'s stale graph-ignore warning (exit 0). `rand`
  (`RUSTSEC-2026-0097`) was the only entry removed from
  `.cargo/audit.toml` in this PR; everything else that left `deny.toml`
  remains an audit-only ignore here.
- 2026-07-31: Corrected the 07-19 pass for `proc-macro-error2`
  (`RUSTSEC-2026-0173`): it is still in `cargo deny`'s resolved graph
  via `matrix-sdk` dev-deps (`aquamarine`) in `zeroclaw-channels`, so it
  is a live deny+audit ignore, not audit-only drift — removed from
  `deny.toml` in the 07-06/07-19 passes, it re-fails `cargo deny check`.
  Restored it in `deny.toml`. Also moved `wasmtime` (`RUSTSEC-2026-0222`)
  to audit-only: it sits behind the optional `plugins-wasmtime` feature,
  so `cargo deny` no longer matches it (`advisory-not-detected`), while
  `cargo audit` still reads the `45.0.3` copy from `Cargo.lock`.
- 2026-07-22: Tightened the "Live, audit-only" intro to state the
  complete removal criterion (crate absent from `Cargo.lock`, *or*
  every locked version patched/unaffected) instead of only the
  crate-absent half. Removed an inaccurate `.cargo/audit.toml` header
  claim that the 20 lockfile-stale entries each have a replacement
  "already landed or in flight" — they are simply audit-only and
  tracked in #8519.
- 2026-07-21: Corrected the `rand` rationale: `Cargo.lock` still
  resolves `rand` 0.8.6, 0.9.4, and 0.10.1, so the crate was never
  absent from the lockfile. The ignore is removed from both files
  because `RUSTSEC-2026-0097` marks all three locked versions as
  patched, not because `rand` disappeared. Reworded the "Resolved"
  category and its process bullet to state the actual removal
  criterion: crate absent from `Cargo.lock`, *or* every locked version
  patched/unaffected per the advisory.
- 2026-07-19: Corrected the 07-06 pass, which removed 20 entries from
  `.cargo/audit.toml` (`unic-*`, `proc-macro-error2`, `derivative`,
  `instant`, `bincode`, `glib`, all 10 GTK3 stack entries) that were
  still present in `Cargo.lock` and still reported by `cargo audit`.
  Restored those 20 as audit-only ignores; they remain removed from
  `deny.toml`, where `cargo deny`'s resolved graph still doesn't need
  them even after the `zeroclaw-desktop` (Tauri) reintroduction in PR
  #8565 (`cargo deny check advisories`/`bans` verified clean). `rand`
  (`RUSTSEC-2026-0097`) stays removed from both files: it is still
  resolved in `Cargo.lock` (0.8.6, 0.9.4, 0.10.1), but the advisory
  marks all three of those versions as patched, so no ignore is
  needed.
- 2026-07-06: Removed advisory ignores from `deny.toml` for crates no
  longer in `cargo deny`'s resolved dependency graph: `unic-*`
  (5 entries), `proc-macro-error2`, `derivative`, `instant`, `bincode`,
  `glib`, all GTK3 stack entries, `rand` (all locked versions patched
  per the advisory), and the `rustls-webpki` entries (0.102.x no longer in resolved
  graph). Remaining deny+audit ignore: `rustls-pemfile` (1). Remaining
  audit-only ignores: `rustls-webpki` (4 entries; in `Cargo.lock` but
  not in resolved dep graph). Cleared the stale `advisory-not-detected`
  warnings `cargo deny` emitted for entries whose crates left its
  resolved graph (a warning class, not a gate failure).
- 2026-07-01: Updated after `upstream/master` merge. Documented that
  the GTK3 stack was resolved by PR #8544 (Tauri desktop removal),
  `proc-macro-error` ignore was dropped, `ttf-parser` is being handled
  by PR #8547, and the `unic-*` group remains blocked by upstream
  `pulldown-cmark` / `mime_guess`. (PR #8543)
- 2026-06-30: Initial doc. Created alongside PR #8542 (wasmtime
  43 → 45.0.3 bump) and PR #8519 (the master audit-tracking issue).
