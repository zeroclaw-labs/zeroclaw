# v0.8.0 Branch Line-Count Report

Comparison of `integration/v0.8.0` (after the excision pass) vs `upstream/master` at `a461b016d`.

## Method

Lines counted via `/tmp/lcount.py` against:

- Rust files under `crates/`, `src/`, `tools/`, `xtask/`, `tests/`, `benches/`, `fuzz/`
- TypeScript / TSX / JS under `web/src/`
- Markdown under `docs/`

Categories:

- **logic** — production source code lines (Rust + TS) excluding the four below
- **tests** — files under `tests/` directories AND `#[cfg(test)] mod` blocks within source files
- **docstrings** — Rust `///` and `//!`, TypeScript `/** ... */`
- **comments** — Rust `//` (non-doc), TypeScript `//` and `/* ... */`
- **docs** — Markdown under `docs/`
- **blank** — whitespace-only lines (excluded from totals where noted)

Excluded: `target/`, `node_modules/`, `dist/`, `.git/`, `tmp/`, `docs/book/book/` (generated mdbook output), `web/src/lib/api-generated.ts` (generated TS bindings), `docs/book/po/` and `po-extract/` (translation artifacts).

## Totals

| category   | master    | branch    | Δ        |
|------------|-----------|-----------|----------|
| logic      | 187,521   | 193,324   | **+5,803** |
| tests      | 118,974   | 123,007   | +4,033   |
| docstrings | 14,765    | 17,100    | +2,335   |
| comments   | 7,572     | 8,894     | +1,322   |
| docs       | 11,512    | 14,131    | +2,619   |
| blank      | 42,590    | 44,138    | +1,548   |
| **TOTAL**  | **382,934** | **400,594** | **+17,660** |

## What this means

The v0.8.0 branch grows the repo by **+17,660 lines total**, of which:

| component | Δ      | share of growth |
|-----------|--------|-----------------|
| logic     | +5,803 | **33%**         |
| tests     | +4,033 | 23%             |
| docs      | +2,619 | 15%             |
| docstrings| +2,335 | 13%             |
| blank     | +1,548 | 9%              |
| comments  | +1,322 | 7%              |

**~67% of the growth is non-logic** (tests, documentation, comments, blank). The hypothesis that "a lot of the code bloat size is docs/comments/doc strings" is supported: actual production code accounts for one-third of the branch's expansion. The branch ships V0.8.0 schema overhaul + multi-agent runtime (#6272) + typed-family providers + peer-auth refactor, and most of the line count went into documenting and testing those changes rather than implementing them.

## Per-area logic growth (sorted by Δ)

| area                       | master logic | branch logic | Δ      |
|----------------------------|--------------|--------------|--------|
| zeroclaw-config            | 12,980       | 17,377       | **+4,397** |
| zeroclaw-runtime           | 38,480       | 39,373       | +893   |
| web                        | 20,742       | 21,518       | +776   |
| zeroclaw-macros            | 912          | 1,581        | +669   |
| zeroclaw-memory            | 5,789        | 6,856        | +1,067 |
| zeroclaw-gateway           | 8,759        | 9,323        | +564   |
| zeroclaw-channels          | 34,556       | 34,576       | +20    |
| zeroclaw-api               | 1,511        | 1,536        | +25    |
| zeroclaw-tui               | 704          | 705          | +1     |
| zeroclaw-tool-call-parser  | 1,168        | 1,168        | 0      |
| zeroclaw-hardware          | 5,760        | 5,760        | 0      |
| zeroclaw-plugins           | 954          | 954          | 0      |
| robot-kit                  | 1,951        | 1,940        | −11    |
| zeroclaw-infra             | 1,024        | 1,021        | −3     |
| src (root binary)          | 6,326        | 5,956        | −370   |
| zeroclaw-tools             | 24,106       | 23,311       | **−795** |
| zeroclaw-providers         | 17,582       | 16,152       | **−1,430** |

## Per-area test growth

| area                       | master tests | branch tests | Δ      |
|----------------------------|--------------|--------------|--------|
| zeroclaw-channels          | 24,825       | 27,392       | +2,567 |
| zeroclaw-config            | 9,673        | 11,562       | +1,889 |
| zeroclaw-runtime           | 30,665       | 31,493       | +828   |
| zeroclaw-memory            | 4,217        | 4,801        | +584   |
| tests (workspace)          | 8,275        | 8,162        | −113   |
| zeroclaw-providers         | 12,140       | 10,838       | **−1,302** |
| zeroclaw-tools             | 15,831       | 15,446       | −385   |
| src (root binary)          | 3,975        | 3,614        | −361   |
| (others < 200 Δ)           |              |              |        |

## Excision-pass contribution

The branch's pre-excision tip would have been roughly **+19,200 total** (extrapolating from the diff between the excision-pass commits — `200b8d6ec..6473818c2` — which net ≈ −1,540 lines). The excision pass accounts for roughly **8% of the branch's net growth being shaved off**.

By category, the excision pass:

- Deleted ~−1,500 lines of dead-on-arrival files, WIP stubs, dead provider helpers + tests, channel-WIP clusters, schema bloat (FeishuConfig fold), Claude Code residue, and dead config sections.
- Added ~+150 lines of new code: V2→V3 migration step for the FeishuConfig fold, three new migration tests, and the audit trail under `docs/maintainers/excision-v0.8.0-incidents.md`.

The deletions explain the negative deltas in `zeroclaw-providers` (−1,430), `zeroclaw-tools` (−795), and the test reductions in providers (−1,302). The +669 in `zeroclaw-macros` is non-excision V3 work (Configurable derive extensions for the schema overhaul).

## Headline

| | |
|---|---|
| Total branch growth | +17,660 |
| Production logic growth | **+5,803 (33%)** |
| Excision pass net | **−1,540** |

The branch is large because V0.8.0 is a large feature delivery, but the production-code surface grew by roughly one-third of the headline number. The remaining two-thirds is documentation, tests, docstrings, comments, and whitespace — the supporting infrastructure for the feature work, not the work itself.
