# Domain DB Incremental Update — Design

> **Status**: Design proposal · awaiting implementation PRs
> **Author**: Kimjaechol + Claude Opus 4.7
> **Date**: 2026-05-02 (rev. 2026-05-02 — distribution scope clarified)
> **Scope**: `src/vault/domain*` + `scripts/build_domain_*.py` + `docs/operations-runbook.md`
> **Non-goals**: changes to `brain.db` (user's private vault — untouched);
> changes to the cross-schema query layer (`unified_search`, `graph_query`)
> beyond a single `meta` read; **publishing the legal corpus from the
> general-public MoA registry** (see §0).

---

## 0. Distribution Policy — General-Public MoA vs. Specialized Forks

This is a **MoA core platform** feature, not a feature exclusive to any
one specialty. The protocol (manifest v2 + delta chain + weekly poll)
ships in the general-public MoA build. **What ships in it does not**.

| Build                                | Domain DB infrastructure | Bundled / pollable corpus |
|--------------------------------------|--------------------------|---------------------------|
| **General-public MoA** (this repo)   | ✅ code present          | ❌ no corpus, no registry URL configured |
| **lawpro** (planned fork)            | ✅ code present          | ✅ `korean-legal` registry pre-configured |
| **medpro** (later fork)              | ✅ code present          | ✅ `korean-medical` registry pre-configured |

Concretely, in the general-public MoA build:

- The code (`vault/domain*`, `vault domain install|update|...` CLI,
  weekly poll, apply-delta) is fully present and tested.
- `[domain].registry_url` defaults to **empty** in `config.toml`. With
  no registry URL, `vault domain update` exits as a no-op and the
  weekly cron task does nothing. No corpus is downloaded, no R2 is
  contacted, no UI surface for "Update legal corpus" is shown.
- A user who wants legal data gets it by either (a) switching to the
  lawpro app or (b) manually pointing `[domain].registry_url` at a
  third-party manifest they trust. The platform does not preconfigure
  one for them.
- Settings UI in the general-public app does not mention "legal" or
  "judicial" as terms; the domain section only appears if a registry
  is configured.

The lawpro fork takes the same codebase, sets
`[domain].registry_url = "https://r2.example.com/moa/domain/korean-legal.manifest.json"`
in its bundled config, enables the corpus-related Settings panel, and
ships the existing law-specific tools (`tools/vault_graph`,
`vault::legal::*`, lawpro document creation skills under
`.claude/skills/document_skills/...`) that are already gated behind the
fork's build flags / .gitignore rules.

This means:

- **Operator-side publication tooling** (`build_domain_db_fast.py`,
  `build_domain_delta.py`, `vault domain publish`, `publish-delta`,
  `stamp-baseline`) lives in this repo because it's needed by both the
  operator-of-MoA persona and the operator-of-lawpro persona, and
  duplicating it across forks invites drift. But the operator running
  the general-public MoA registry has no corpus to publish; the binary
  and scripts are simply available for forks that do.
- **Client-side update logic** is a single code path used by both
  builds. The only difference is whether `[domain].registry_url` is
  set in the bundled config.
- **Tests** in this repo cover the protocol end-to-end with a mock
  legal corpus fixture (no real legal data). The lawpro fork adds an
  integration test against its real registry; that test does not run
  here.

**Why infrastructure here, corpus there**: keeping the protocol in
core means lawpro/medpro forks rebase against MoA without diverging
on a 1,500 LOC subsystem. Keeping the corpus out means the general
public MoA app stays small, generic, and free of legal data on disk.

---

## 1. Problem Statement

`domain.db` ships the legal/medical/etc. corpus that backs MoA's Second
Brain. Today it is **swap-by-replace**: `vault domain install` downloads
the entire ~1.5 GB bundle, atomic-renames it into place, and any prior
file is gone.

That model fails the operational reality of the corpora:

| Corpus            | Change cadence                  | Typical weekly delta |
|-------------------|---------------------------------|----------------------|
| Korean cases      | Continuous, ~tens to hundreds   | 4–40 MB              |
| Korean statutes   | A few times per year            | 0–2 MB               |
| Medical (future)  | Slow, periodic                  | < 5 MB               |

For an end user, "always re-download 1.5 GB" is unacceptable on mobile
data and slow connections. For the operator, "always re-publish 1.5 GB"
wastes CDN egress when most weeks change nothing.

We need:

1. **End-user clients poll on a fixed schedule** (default: weekly), but
   download only what changed since the installed version.
2. **Operator publishes when there is something to publish** —
   sometimes weekly, sometimes monthly, sometimes never. The protocol
   must accept "operator was silent for six weeks" as the normal case.
3. **Annual baseline reset** (default: every January 15th) so the
   delta chain stays bounded and clients that fall arbitrarily far
   behind always have a path forward.
4. **Zero-byte fast path**: when the operator hasn't published anything
   new, the client downloads the manifest (a few KB) and stops.

---

## 2. Data Model

### 2.1 Manifest v2

The manifest grows from a single bundle pointer to a baseline + delta
chain. Schema version bumps to `2`; v1 manifests stay readable for
backward compatibility (operator-side, see §6).

```jsonc
{
  "schema_version": 2,
  "name": "korean-legal",
  "version": "2026.04.22",
  "generated_at": "2026-04-22T00:00:00Z",
  "generator": "zeroclaw 0.1.8",

  "baseline": {                              // mandatory
    "version": "2026.01.15",                 // immutable until next annual cut
    "url":    "https://r2.example.com/moa/domain/korean-legal-2026.01.15.db",
    "sha256": "…64 hex…",
    "size_bytes": 1_487_239_104,
    "stats": { "vault_documents": 412_018, "vault_links": 2_841_006 }
  },

  "deltas": [                                // ordered, oldest → newest; may be empty
    {
      "version": "2026.01.22",
      "applies_to_baseline": "2026.01.15",   // must equal baseline.version above
      "url":    "https://r2.example.com/moa/domain/korean-legal-delta-2026.01.22.sqlite",
      "sha256": "…64 hex…",
      "size_bytes": 4_281_344,
      "ops":    { "upsert": 5, "delete": 0 },
      "generated_at": "2026-01-22T00:00:00Z"
    },
    {
      "version": "2026.04.22",
      "applies_to_baseline": "2026.01.15",
      "url":    "https://r2.example.com/moa/domain/korean-legal-delta-2026.04.22.sqlite",
      "sha256": "…",
      "size_bytes": 38_201_984,
      "ops":    { "upsert": 412, "delete": 3 },
      "generated_at": "2026-04-22T00:00:00Z"
    }
  ]
}
```

**Cumulative, not incremental, deltas.** Each delta entry contains
*all* changes since `baseline.version`, not just changes since the
previous delta. A new client (or one that has been offline for 11
weeks) downloads exactly one delta — the latest — and is fully
caught up. The trade-off is that delta files grow over the year;
the annual baseline cut bounds them. See §3.

**Manifest-only `version`** at the top level is the *publication
identity*: equal to the newest delta's version, or the baseline's
version when `deltas == []`. Clients use this for their "am I
current?" check and never need to scan the array.

### 2.2 Delta File Format

A delta is a small SQLite file with the **same vault schema** as the
domain DB plus one extra table. Reusing the schema means the
operator-side builder can use the existing `vault_documents` insert
paths verbatim, and the client applies the delta with `ATTACH delta AS
d; INSERT OR REPLACE INTO domain.* SELECT … FROM d.*; …`.

```sql
-- in addition to the standard vault tables (documents/links/aliases/tags/frontmatter/embeddings/fts):
CREATE TABLE vault_deletes (
    uuid       TEXT NOT NULL PRIMARY KEY,    -- vault_documents.uuid that no longer exists
    deleted_at INTEGER NOT NULL              -- unix seconds
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Required keys:
--   schema_kind          = 'domain-delta'
--   delta_version        = '2026.04.22'
--   applies_to_baseline  = '2026.01.15'
--   baseline_sha256      = '<64 hex>'   -- sanity check before apply
```

Tables present in a delta hold **only the rows that changed**. A row
in `vault_documents` represents an upsert (the client `INSERT OR
REPLACE`s by uuid); a row in `vault_deletes` removes the document and
its dependents.

### 2.3 Domain DB `meta` Table

`domain.db` learns a tiny `meta` table so the client can answer
"what's installed?" without reading the manifest:

```sql
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Keys written by install / apply_delta:
--   schema_kind        = 'domain'
--   baseline_version   = '2026.01.15'
--   baseline_sha256    = '<64 hex>'    -- copied from manifest at install time
--   current_version    = '2026.04.22'  -- = baseline_version when no delta applied
--   last_applied_at    = '<unix sec>'
```

The schema migration is additive: `CREATE TABLE IF NOT EXISTS meta`.
Old `domain.db` files without `meta` are treated as `current_version
= baseline_version = "unknown"`, which forces a one-time full
re-install on next update. (Fine: these are the ones we want to
upgrade anyway.)

---

## 3. Update Algorithm (Client)

```
fn vault_domain_update(workspace_dir, registry_url) -> Result<UpdateOutcome>:
    manifest = fetch(registry_url)            // ~1–10 KB, always
    validate(manifest)                        // schema_version == 2, sha lengths, ...

    let installed = read_meta(workspace_dir)  // None when meta table missing

    // ── Decision tree ───────────────────────────────────────────────
    match installed:
        None
          | Some(m) if m.baseline_version != manifest.baseline.version:
            → FullInstall(manifest.baseline)               // §3.1

        Some(m) if m.current_version == manifest.version:
            → AlreadyCurrent                                // §3.2 — zero-byte path

        Some(m):
            → ApplyDelta(latest_delta_for(manifest))        // §3.3
```

Three outcomes, three byte budgets, no other branches.

### 3.1 FullInstall — annual baseline cut, or first install

Triggered when no domain.db is installed, or when its
`baseline_version` no longer matches the manifest. Streams
`manifest.baseline.url`, verifies sha256 + size, atomic-renames in
via existing `domain::install_from`, then writes the `meta` table:

```sql
INSERT OR REPLACE INTO meta(key,value) VALUES
  ('schema_kind','domain'),
  ('baseline_version', :baseline_version),
  ('baseline_sha256',  :baseline_sha256),
  ('current_version',  :baseline_version),
  ('last_applied_at',  :now);
```

This is what runs on the annual January-15 cut for everyone, and on
every new user's first install.

### 3.2 AlreadyCurrent — zero-byte fast path

`installed.current_version == manifest.version`. The client downloads
nothing else and reports "domain corpus up to date (`v2026.04.22`,
applied 9 days ago)". This is the path for the **vast majority of
weekly checks** — operator was silent that week, or the user already
caught up earlier in the week.

### 3.3 ApplyDelta — catch up from N weeks behind

`installed.baseline_version == manifest.baseline.version` but
`installed.current_version != manifest.version`. The client picks the
**newest** delta in `manifest.deltas` (always the last entry, since
deltas are cumulative — see §2.1) and:

1. Downloads `delta.url`, verifies sha256 + size.
2. Verifies `delta.applies_to_baseline == installed.baseline_version`.
   On mismatch → fall back to FullInstall (paranoid; should never
   happen if §3.1 ran correctly).
3. Detaches the live `domain` schema (existing `domain::detach`).
4. Applies the delta in a single SQLite transaction:

   ```sql
   ATTACH DATABASE 'delta.sqlite' AS d;
   BEGIN;
     -- Upserts (every changed/added row).
     INSERT OR REPLACE INTO main.vault_documents
       SELECT * FROM d.vault_documents;
     INSERT OR REPLACE INTO main.vault_links
       SELECT * FROM d.vault_links;
     INSERT OR REPLACE INTO main.vault_aliases
       SELECT * FROM d.vault_aliases;
     INSERT OR REPLACE INTO main.vault_frontmatter
       SELECT * FROM d.vault_frontmatter;
     INSERT OR REPLACE INTO main.vault_tags
       SELECT * FROM d.vault_tags;
     INSERT OR REPLACE INTO main.vault_embeddings
       SELECT * FROM d.vault_embeddings;
     -- Hard deletes (statute repealed, case withdrawn, …).
     DELETE FROM main.vault_links     WHERE source_doc_id IN
       (SELECT id FROM main.vault_documents WHERE uuid IN (SELECT uuid FROM d.vault_deletes));
     DELETE FROM main.vault_aliases   WHERE doc_id IN
       (SELECT id FROM main.vault_documents WHERE uuid IN (SELECT uuid FROM d.vault_deletes));
     DELETE FROM main.vault_frontmatter WHERE doc_id IN
       (SELECT id FROM main.vault_documents WHERE uuid IN (SELECT uuid FROM d.vault_deletes));
     DELETE FROM main.vault_tags      WHERE doc_id IN
       (SELECT id FROM main.vault_documents WHERE uuid IN (SELECT uuid FROM d.vault_deletes));
     DELETE FROM main.vault_embeddings WHERE doc_id IN
       (SELECT id FROM main.vault_documents WHERE uuid IN (SELECT uuid FROM d.vault_deletes));
     DELETE FROM main.vault_documents WHERE uuid IN (SELECT uuid FROM d.vault_deletes);
     -- Bookkeeping.
     UPDATE main.meta SET value = :delta_version WHERE key = 'current_version';
     UPDATE main.meta SET value = :now           WHERE key = 'last_applied_at';
   COMMIT;
   DETACH DATABASE d;
   ```

   The whole apply is one SQLite transaction inside the
   freshly-opened domain DB connection. A crash mid-apply leaves the
   pre-delta DB intact (SQLite WAL guarantees), so the client can
   re-run on the next poll and try again.

5. Re-attaches `domain.db` to the live process connection.
6. (Optional, behind a flag for v2.1) `PRAGMA optimize` and incremental
   vacuum — only when accumulated weekly deltas exceed ~50% of
   baseline size, which the annual cut already prevents.

### 3.4 Schedule

Client wakes once per week (default: Sunday 03:00 local) and runs the
update **only if `[domain].registry_url` is set**. Override the cron
via `[domain].update_cron` in `config.toml`. A manual
`vault domain update` always works the same way — when no registry is
configured it prints "no domain registry configured (skipped)" and
exits 0.

The operator's publication cadence is **independent**. A configured
client polls every week; the operator publishes when there is
something to publish, which in practice means "occasionally" — empty
weeks resolve to AlreadyCurrent and zero bytes downloaded.

**General-public MoA**: ships with `[domain].registry_url` empty (see
§0). The weekly task is registered but no-ops on every wake-up. No
network traffic, no UI surprise. The user has to opt in by editing
config or by switching to a specialized fork (lawpro / medpro) whose
bundled config points at a real manifest URL.

---

## 4. Publication Algorithm (Operator)

### 4.1 Annual baseline (January 15, or any time the operator decides)

```bash
# 1. Build the full corpus into a fresh DB.
python scripts/build_domain_db_fast.py \
       --corpus-dir corpus/legal \
       --out  out/korean-legal-2026.01.15.db

# 2. Stamp baseline meta into the DB itself.
zeroclaw vault domain stamp-baseline \
       --db   out/korean-legal-2026.01.15.db \
       --version 2026.01.15

# 3. Upload to R2 and emit a v2 manifest with deltas: [].
zeroclaw vault domain publish \
       --baseline out/korean-legal-2026.01.15.db \
       --baseline-version 2026.01.15 \
       --baseline-url https://r2.example.com/moa/domain/korean-legal-2026.01.15.db \
       --out-manifest out/korean-legal.manifest.json
```

Resulting manifest: `baseline = {…2026.01.15…}`, `deltas = []`.
Every existing client hits §3.1 and re-downloads. This happens once
a year (or whenever the operator wants a clean slate — e.g. schema
migration that breaks delta compatibility).

### 4.2 Periodic delta publication (whenever the operator has changes)

```bash
# 1. Build a fresh full DB from the latest corpus.
python scripts/build_domain_db_fast.py \
       --corpus-dir corpus/legal \
       --out  out/staging-2026.04.22.db

# 2. Diff against the published baseline → emit the cumulative delta.
python scripts/build_domain_delta.py \
       --baseline out/korean-legal-2026.01.15.db \
       --current  out/staging-2026.04.22.db \
       --out      out/korean-legal-delta-2026.04.22.sqlite \
       --version  2026.04.22 \
       --applies-to-baseline 2026.01.15

# 3. Upload + refresh manifest. The publish command appends the new
#    delta to manifest.deltas and bumps top-level version.
zeroclaw vault domain publish-delta \
       --delta out/korean-legal-delta-2026.04.22.sqlite \
       --delta-url https://r2.example.com/moa/domain/korean-legal-delta-2026.04.22.sqlite \
       --in-manifest  out/korean-legal.manifest.json \
       --out-manifest out/korean-legal.manifest.json
```

The diff (`build_domain_delta.py`) is straightforward because the
schema is shared:

```python
# pseudo
upserts  = SELECT cur.* FROM cur LEFT JOIN base ON cur.uuid=base.uuid
           WHERE base.uuid IS NULL OR base.checksum != cur.checksum
deletes  = SELECT base.uuid FROM base LEFT JOIN cur ON base.uuid=cur.uuid
           WHERE cur.uuid IS NULL
```

Always diff against **the published baseline**, never against the
prior delta. Cumulative, not chained.

### 4.3 Silent weeks

The operator does nothing. The previous manifest stays live on R2.
Clients fetch it, hit §3.2 AlreadyCurrent, download zero bytes.
This is the common case and the protocol is built around it.

### 4.4 Pruning old deltas

`manifest.deltas` keeps growing for a year. Two options:

- **Keep all** — at year-end, around 50 entries, ~10 KB of JSON. Fine.
- **Keep latest only** — since deltas are cumulative, only the last
  one is ever downloaded. Older ones can be deleted from R2 once a
  newer one is published (and from the manifest array).

Recommend **keep latest only** for storage efficiency, with a small
operator-side hold (e.g. retain the previous one for 7 days for
rollback). The client only ever reads the last entry, so this is
invisible to them.

---

## 5. Integrity & Failure Modes

| Risk                                                         | Mitigation |
|--------------------------------------------------------------|------------|
| Corrupt download                                             | sha256 + size verified before any DB write (existing `download_bundle`). |
| Delta references a baseline the client isn't on              | `applies_to_baseline` mismatch → client falls back to FullInstall. |
| Mid-apply crash                                              | Single transaction → SQLite WAL rolls back; next poll retries. |
| Operator publishes a v2 manifest while client is on v1 build | Old client hits "schema_version mismatch" error and falls back to legacy install path (see §6). |
| Manifest URL serves stale-cached v1 after v2 cut             | Cache-control headers on R2; client retries once after 30 s if it sees v1 + a domain.db with v2 meta. |
| Two devices update concurrently                              | Local file lock around the apply transaction; second waits, sees `AlreadyCurrent`, exits. |
| User on mobile data fires `update` on 40 MB delta            | Existing `download_bundle` is sync-blocking; UI must show progress. v2.1 — switch to streaming with resume. |

---

## 6. Backward Compatibility & Migration

- **Old clients (v1 manifest expected)** keep working until the first
  v2 manifest is published. We can hold the v1 manifest at a
  parallel URL (`registry.v1.json`) for one release cycle so old
  builds keep updating in the meantime. The Tauri auto-updater is
  expected to bring everyone to a v2-aware build before the next
  baseline cut.
- **Old `domain.db` files (no `meta` table)** auto-trigger §3.1
  FullInstall on first v2 update. One-time cost, makes everyone
  uniform.
- **Schema breaking changes** (new columns in `vault_*`) ride on the
  annual baseline. Mid-year delta files keep the same schema as the
  in-place baseline; new columns wait until next January.

---

## 7. Implementation Plan

Three sequential PRs, each individually merge-able and testable.

### PR 1 — Manifest v2 + `meta` table (foundation)

- `domain_manifest.rs` — add `schema_version: 2`, `BaselineSpec`,
  `Vec<DeltaSpec>`. Keep v1 deserializer for read-only compat.
- `vault/schema.rs` — `CREATE TABLE IF NOT EXISTS meta` migration.
- `vault/domain.rs` — `read_meta()` / `write_meta()` helpers.
- `domain_cli.rs install` — write meta on full install.
- `config/schema.rs` — `[domain].registry_url: Option<String>` with
  default `None`; `domain_cli.rs update` short-circuits to a friendly
  no-op when unset. **The general-public MoA ships with this unset
  (§0); lawpro/medpro forks override it in their bundled config.**
- Tests: v2 parse, v1 fallback, meta round-trip, install populates
  meta, `update` with no `registry_url` exits 0 silently and touches
  no files.

**Approx**: ~250 LOC + ~150 LOC tests. Ships behind no flag — pure
data-model groundwork. Client behaviour unchanged from a user
perspective (no corpus configured ⇒ nothing happens; corpus
configured ⇒ existing v1 install path still works until PR 2).

### PR 2 — Apply-delta path (client)

- New `vault/domain_delta.rs` — apply-delta transaction, sanity
  checks (`applies_to_baseline`, sha, size).
- `domain_cli.rs update` — replace the `install`-call shim with the
  decision tree from §3.
- UI button → existing `update` command (no change).
- Tests: full install, zero-byte AlreadyCurrent, apply 1 delta,
  sha-mismatch refuses, baseline-mismatch falls back, mid-apply
  rollback (synthetic crash via test hook).

**Approx**: ~350 LOC + ~400 LOC tests.

### PR 3 — Operator-side builders + publish CLI

- `scripts/build_domain_delta.py` — diff baseline ↔ current → emit
  delta SQLite + meta rows.
- `vault/domain_cli.rs publish` (extend) and `publish-delta` (new)
  → upload helper + manifest mutation.
- `vault/domain_cli.rs stamp-baseline` — write baseline meta into a
  freshly-built DB.
- `docs/operations-runbook.md` — operator steps for both annual cut
  and weekly check-and-maybe-publish.
- Tests: builder produces a delta the client can apply (round-trip
  test that consumes PR 2's apply path).

**Approx**: ~200 Python + ~150 Rust + ~250 LOC tests.

Total budget across the three PRs: ~1,500 LOC + ~800 LOC tests.

---

## 8. Open Questions

1. **Delta retention policy** — keep latest only (§4.4), or keep last
   N for rollback? Recommend latest+1 with 7-day overlap.
2. **Compression** — manifest reserves `compression: "none" | "zstd"`.
   Deltas are SQLite, not very compressible (sqlite already
   bit-packs); baseline compresses ~30% with zstd. Worth doing on
   baseline, skip on deltas.
3. **Multi-domain coexistence** — current scope is single
   `domain.db`. Medical/legal coexistence is out of scope here and
   handled by the existing single-corpus-at-a-time switch (operator
   publishes one manifest per corpus, user picks one).
4. **Auto-update opt-out** — should `[domain].auto_update = false`
   suppress the weekly poll for users on metered connections? Yes,
   default `true`, expose in Settings.
5. **Telemetry** — log update outcomes (full / delta / already-current
   / failed) to existing observability sink? Recommend yes, no PII.
   The general-public build's no-op branch (§0, §3.4) should not log
   anything at all — silence is part of the contract.
6. **Fork-time config flip** — should lawpro's bundled config also
   gate the `tools/vault_graph` and `vault::legal::*` *runtime*
   surface so the general-public build doesn't expose legal-specific
   tools to its agent loop? Recommend yes, but that work is part of
   the lawpro fork bring-up, not this PR series.

---

## 9. Acceptance Criteria

- [ ] **General-public MoA build** with `[domain].registry_url`
      unset: weekly cron fires, hits the no-op branch, makes zero
      network requests, leaves the filesystem untouched. No domain
      Settings panel is visible in the UI.
- [ ] **Configured client** polling on a manifest with `deltas: []`
      and a matching installed `current_version` performs **zero**
      body downloads (only the manifest GET).
- [ ] Configured client one baseline behind downloads exactly the
      latest delta and arrives at `current_version == manifest.version`.
- [ ] Configured client on a stale baseline downloads the new
      baseline (one bundle, no deltas yet) and arrives correctly.
- [ ] Operator publishing a delta on top of an unchanged baseline
      produces a manifest a v2 client accepts and a v1 client refuses
      cleanly (no corruption, just an error).
- [ ] Mid-apply process kill leaves the prior `current_version`
      intact and the next poll recovers.
- [ ] Annual baseline cut: every existing configured client moves to
      the new baseline on its next weekly poll.

---

## 10. References

- Existing modules: `src/vault/domain.rs`, `src/vault/domain_manifest.rs`,
  `src/vault/domain_migrate.rs`, `src/vault/domain_cli.rs`,
  `src/vault/schema.rs`.
- Patent context: §6D MoA Vault — Second Brain (`docs/ARCHITECTURE.md`),
  domain DB is the operator-curated half of the second brain;
  `brain.db` is the user-curated half and is **not** affected by any
  of the above.
- Related ops doc: `docs/operations-runbook.md` (will gain an
  operator section in PR 3).
