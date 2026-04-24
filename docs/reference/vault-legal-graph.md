# Vault · Legal Graph

Second-brain reference: a Korean-legal-domain graph database layered on
the existing vault (`src/vault/`). Ingests statute and precedent
markdown, extracts citations deterministically, and exposes the result
as a searchable graph through three independent paths — Agent Tools,
HTTP endpoints, and static HTML snapshots.

Last verified: **April 23, 2026**.

---

## Mental model

```
local markdown                      brain.db (single SQLite file)            agent / gateway / file
──────────────           ┌───────────────────────────────────────┐           ──────────────────────
 법령/*.md                │  vault_documents (one row per article) │
 판례/*.md    ─ ingest ─> │  vault_links     (cites / ref-case /  │
                          │                   internal-ref /       │
                          │                   cross-law)           │
                          │  vault_aliases   (근기법 제43조 등)    │
                          │  vault_frontmatter, vault_tags         │
                          └───────────────────────────────────────┘
                                           │ read-only
                                           ▼
                            src/vault/legal/graph_query.rs
                            (iterative BFS, MAX_NODES=500)
                                           │
                     ┌─────────────────────┼─────────────────────┐
                     ▼                     ▼                     ▼
                5 agent tools         HTTP + viewer       HTML/JSON snapshot
                (Phase 2)             (Phase 3)           (Phase 4)
```

All three consumers share the **same** graph-query module — BFS rules,
kind filters, node caps, and alias canonicalisation are defined once
and applied uniformly.

---

## Slug conventions (canonical IDs)

Every node in the legal graph has a canonical slug, stored as
`vault_documents.title`:

| Kind | Slug format | Example |
|---|---|---|
| Statute article | `statute::{법령명}::{N}[-M]` | `statute::근로기준법::43-2` |
| Case | `case::{사건번호}` | `case::2024노3424` |

- `{법령명}` is always the **canonical long name** (see [law aliases](#law-name-aliases) below).
- Sub-articles (조의 N) use hyphen separator: `제43조의2` → `43-2`.
- Paragraph / sub-paragraph refs (제1항, 제5호) are captured as edge
  metadata, not node keys — they don't split the graph.

---

## Edge relations

Stored in `vault_links.display_text` (`link_type` is always `wikilink`
to fit the existing CHECK constraint):

| Relation | Source → Target | Meaning |
|---|---|---|
| `cites` | case → statute | 참조조문 — the direct link between 판례 and 법령 |
| `ref-case` | case → case | 참조판례 — precedent references |
| `internal-ref` | statute → statute (same law) | 조문 내부 상호참조 (예: 제109조 → 제36조) |
| `cross-law` | statute → statute (different law) | 법령 간 인용 (예: 근로기준법 → 근퇴법) |

Evidence (the actual citing substring) is in `vault_links.context`.

---

## CLI

### Ingest

```bash
# Walk a directory of .md files; routes each to the statute or case
# extractor based on content ("## 사건번호" → case, JSON meta → statute).
zeroclaw vault legal ingest ~/law/markdown

# Parse-only, no DB writes (useful for smoke-testing extractor coverage).
zeroclaw vault legal ingest ~/law/markdown --dry-run
```

Idempotent: per-file checksum short-circuits unchanged files; changed
files have their links/tags/frontmatter replaced atomically in a
transaction.

After the batch, a `resolve_pending_links` pass wires up edges whose
target was ingested later in the same batch.

### Stats

```bash
zeroclaw vault legal stats
```

### Export a subgraph

```bash
# Standalone HTML (offline Cytoscape viewer with data embedded inline).
zeroclaw vault legal export \
  --root "statute::근로기준법::36" --depth 2 \
  --kinds statute,case --out ./graph.html

# graphify-compatible JSON (`{nodes, edges, __meta}`).
zeroclaw vault legal export \
  --root "case::2024노3424" --depth 2 \
  --out ./subgraph.json --format json
```

`--depth` is clamped 1-3 (deeper walks on a well-connected node can
reach hundreds of articles). `--kinds` is optional; omit for both.

---

## Agent tools

Registered automatically in the default registry (`all_tools_with_runtime`).
All are read-only and `safe_for_slm=true`.

### `legal_graph_find`

Resolve a human-readable reference to a canonical slug.

```json
{ "query": "근로기준법 제43조의2(체불사업주 명단 공개)" }
```

Lookup cascade (cheap → expensive, short-circuits on exact match):

1. Exact `vault_documents.title` (slug form)
2. Exact `vault_aliases.alias` (e.g. `근기법 제43조의2`)
3. Parse as statute citation via the same regex used for body scanning
4. Parse as bare case number
5. FTS5 trigram fallback on full content

Each hit carries a `matched_via` tag so the agent can weigh confidence.

### `legal_graph_neighbors`

```json
{ "node": "statute::근로기준법::36", "depth": 1, "kinds": ["case"] }
```

Returns a human-readable summary of reachable nodes and edges (both
directions: cited-by + citing).

### `legal_graph_shortest_path`

```json
{ "from": "case::2024노3424", "to": "statute::근로기준법::109", "max_depth": 4 }
```

BFS undirected hop-count. Returns the slug sequence or `null` if no
path within `max_depth`.

### `legal_graph_subgraph`

Same input as `neighbors`, but returns the full graphify-compatible
JSON payload (`{nodes, edges, truncated}`). For programmatic handoff
(to viewer / snapshot exporter / external tooling).

### `legal_read_article`

```json
{ "slug": "case::2024노3424", "sections": ["판결요지", "참조조문"] }
```

Returns the full body of a node. For statutes: article header + body
text + frontmatter. For cases: full markdown (or filtered `##`
sections if `sections` is specified). Use before quoting legal
language so the agent can cite verbatim.

---

## HTTP endpoints

Served by the main gateway when `zeroclaw gateway` is running. Read-only.

| Method | Path | Returns |
|---|---|---|
| `GET` | `/api/legal/graph/subgraph?node=<slug>&depth=<N>&kinds=<csv>` | `{nodes, edges, truncated}` |
| `GET` | `/api/legal/graph/path?from=<slug>&to=<slug>&max_depth=<N>` | `{path: […] \| null}` |
| `GET` | `/api/legal/graph/stats` | counts |
| `GET` | `/legal-graph/viewer[?node=…&depth=…&kinds=…]` | Cytoscape viewer (HTML) |

Each request opens a fresh `SQLITE_OPEN_READ_ONLY` connection; SQLite
WAL handles concurrent readers.

### Viewer UX

- Node-kind colour-coding (statute = blue, case = amber)
- Dashed edges for unresolved links
- Click a node → detail pane shows metadata + all incident edges with
  evidence snippets
- **Double-click** a node → re-root at that slug (graph-walk UX)
- "Export JSON" downloads the current view as graphify JSON
- Deep-link via URL params for agent handoff

---

## Static HTML snapshot

`zeroclaw vault legal export --out graph.html` produces a **single
self-contained file** with the Cytoscape viewer and data embedded
inline — no gateway, no network, no dependencies except a CDN-loaded
`cytoscape.min.js`. Share as email attachment or cold archive.

Injection security: the JSON payload has `</` escaped to `<\/` so a
malicious citation can't break out of the embedding script tag.

---

## Extraction pipeline

### Statute markdown

Expected shape (produced by user's ingest pipeline from
`국가법령정보 API`):

```md
# 근로기준법

- **MST(법령 마스터 번호)**: 276849
- **공포일자**: 20251001
- **시행일**: 20251001

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", …},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J36:0", "number": "제36조(금품 청산)", "text": "…"},
    {"anchor": "J43:2", "number": "제43조의2(체불사업주 명단 공개)", "text": "…"}
  ],
  "supplements": [{"title": "부      칙…", "body": "…"}]
}
```
```

The JSON block is authoritative; the extractor parses it structurally,
then runs the citation regex on each article's `text` field with the
outer law name as the "current-law" context.

### Case markdown

Expected shape:

```md
## 사건번호
2024노3424

## 선고일자
20250530

## 법원명
수원지방법원

## 참조조문
형사소송법 제327조 제6호, 근로기준법 제36조, 제109조

## 참조판례
대법원 2012. 9. 13. 선고 2012도3166 판결

## 판결요지
…
```

Filename is also parsed:
`{case#}_{catcode}_{kind}_{serial}_{verdict}_{court}_{casename}.md`.
Parent directory name is the 선고일자 (`YYYYMMDD`).

### Citation regex

Two layers:

1. **Anchors** — law-name markers that set context:
   - `「법령명」` bracketed
   - `{법령명|short form} 제N조` unbracketed
   Each anchor passes the matched name through `law_aliases::canonical_name`
   before storing, so `근기법`, `근로기준법`, `근로자퇴직급여 보장법`
   all canonicalise correctly.
2. **Bare article refs** — `제N조[의M][제K항][제L호]` inherit the
   most recent anchor's law name. `initial_law` param on
   `extract_statute_citations` handles the "current-law" case inside
   a statute's own body.

Case numbers: `YYYY{유형}NNNNN` where 유형 is 1-3 Korean chars
(covers civil/criminal/constitutional/administrative — 다/노/도/가단/
고정/헌바 etc.).

---

## 행위시법 원칙 (time-of-action law) — temporal metadata

Korean law follows **행위시법 원칙**: the version of a statute in force
**at the time the incident occurred** is the one that applies to a case
— not the version at the time of judgment. The ingester therefore
extracts and stores three kinds of dates so a practitioner (or agent)
can match them:

| Dimension | Source | Frontmatter key | Format |
|---|---|---|---|
| **법령 시행일** | 부칙 body (`제1조(시행일) 이 법은 …부터 시행한다`) | `effective_date` on `statute_supplement` node | YYYYMMDD |
| **조문 최신 개정일** | `<개정 YYYY. M. D., …>` tags inside each article body | `amendment_dates` (CSV) + `latest_amendment_date` on `statute_article` node | YYYYMMDD |
| **판례 사건발생일** | Date literals in `## 판례내용` (판결이유) | `incident_dates` (CSV, up to 50) + `incident_date_earliest` + `incident_date_latest` on `case` node | YYYYMMDD |

### 시행일 parsing

`parse_supplement_effective_date(body, promulgation_date)` handles the
three phrasings seen in published 부칙 bodies:

1. **Explicit literal** — `이 법은 2025년 10월 1일부터 시행한다` →
   extracts the date literal near `시행`.
2. **Promulgation-day** — `이 법은 공포한 날부터 시행한다` →
   returns `promulgation_date` (supplied by the caller from the
   parsed supplement title's `법률 제N호, YYYY. M. D.`).
3. **Relative offset** — `이 법은 공포 후 6개월이 경과한 날부터 시행한다`
   → `promulgation_date + 6 months` (also handles `N일`).

The parser **scopes to the primary `제1조(시행일)` clause** when one is
present, so later `다만, 제XX조의 개정규정은 2026년 1월 1일부터 시행한다`
exceptions don't override the main date. Per-article exception handling
is a known follow-up (see "Not automatic yet").

### 사건발생일 parsing

`find_all_dates(body)` collects every date literal matching any of:

- `2024. 3. 28.` / `2024.3.28` / `2024. 3. 01.`
- `2024년 3월 28일`
- `2024-03-28`

Invalid month/day combinations (e.g. `2024. 13. 45.`) are dropped; the
result is sorted + deduplicated. The ingester stores up to 50 dates
per case in the CSV; `incident_date_earliest` / `incident_date_latest`
carry the first and last for common range queries.

The case-extractor deliberately scans `## 판례내용` first and falls
back to the full markdown so it picks up dates in 판시사항 / 판결요지
when the verdict body is short.

#### Fallback: 1심 사건번호의 접수년도 (filing-year fallback)

Supreme Court and many appellate judgments omit explicit incident
dates in their reasoning section because the 1st-instance judgment
already carried them. When the body contains no date literal at all,
the ingester **infers the incident year** from referenced case
numbers:

- Every Korean case number's leading 4 digits = **소장 접수 년도**
  (the year the complaint / indictment was filed). Per domain
  convention this year tracks the 사건발생 year closely enough that
  it can proxy the `effective_date` match.
- `infer_filing_year_from_case_refs(body, own_case_number)`:
  1. Extracts every case number in the body via `extract_case_numbers`
  2. Excludes the judgment's own case number
  3. Takes the **earliest** year among the remainder (lowest court /
     original filing)
  4. Sanity-checks the year against `1950 ≤ year ≤ current+1`
  5. Falls back to the own-case year if no other references survive

When this fallback fires, the ingester stores `{year}0101` as
`incident_date_earliest` and `{year}1231` as `incident_date_latest`
— sentinel values that keep range queries functional — plus an
explicit provenance tag:

- `incident_date_source = "body"` → dates came from date literals,
  high confidence
- `incident_date_source = "filing_year_fallback"` → single year
  inferred from case references, ±1 year tolerance recommended
- `incident_date_source = "none"` → no reliable signal; match by
  `verdict_date` or widen the time window

### Applying the principle — 제1원칙 (primary rule)

**제1원칙 (primary rule).** When a judgment's 적용법조 / 적용법령
section names a law **without** the `구` prefix — i.e. plain
`형법`, `민법`, `근로기준법`, `형사소송법` etc. — **the applicable
version is the one in force at `verdict_date` (판결 선고 시점)**.
This is the default; it covers the vast majority of citations.

```
plain law name in 적용법조
   → applicable version := supplement whose
         effective_date ≤ case.verdict_date
         AND no later supplement precedes it
   → 100% reliable (by Korean adjudication convention)
```

The judge has ALREADY decided whether to apply the current law
(including any 형사 경한-개정 예외). Our job is just to honour that
written decision. No incident-date-based inference, no range matching,
no semantic comparison in the code path.

**예외 — `구 {법}(…개정되기 전의 것)` citations.** The judge marked
this specific citation as a pre-revision reference. Use Tier 1:

```
citation prefixed with `구 {법}(YYYY. M. D. 법률 제N호로
                                    개정되기 전의 것) 제N조`
   → edge.revision_cutoff = YYYYMMDD  (populated by extractor)
   → applicable version := supplement whose
         effective_date ≤ cutoff − 1 day
         AND no later supplement precedes it
   → 100% reliable
```

That's the entire applicable-law determination. Two branches, zero
inference. `incident_date_*` fields are **not** inputs to this
decision — they serve a different purpose (see next subsection).

### Relationship to 형사 경한-개정 예외

**Not a separate concern.** Per Korean adjudication convention, when
the judgment decides that the newer (more lenient) law applies to a
criminal matter, the 적용법조 section writes the plain current law
name — which falls straight into Tier 2. When instead the pre-
revision version applies (the case where no lenient-amendment kicked
in), the 적용법조 section writes `구 {법}(…)` — Tier 1.

The judge has already made the lenient-or-not judgment; we simply
follow their formatting choice. No semantic comparison needed in the
code.

### Why incident_date_* fields still exist

- `case.incident_date_earliest` / `incident_date_latest` /
  `incident_date_source` let agents answer "when did the dispute
  happen?" for timeline / narrative use cases.
- They also support **non-applicable-law** temporal queries like
  "show me all judgments whose incidents were in Q2 2024".
- They are NOT inputs to the applicable-version picker.

The `wonsim_offset` / `own_case_year` fallbacks discussed above remain
useful when the body carries no date literal at all — but only for
the "when" question, not the "which law version" question.

### The 형사사건 exception (유일한 예외)

In **criminal cases only** (형사사건), if the law has been amended
between the 행위시 and the 판결시 to be **more lenient**
(경하게) — lower penalty, narrower element, etc. — then the current
version applies instead of the 행위시 version. Civil and
administrative cases have no such exception.

Detection requires:

1. Knowing the case is criminal — carried by `case_type_name`
   frontmatter (`"형사"`) plus `court_type_code` patterns.
2. Comparing penalty clauses across the two versions — textual
   comparison at best, requires LLM judgment for non-trivial changes.

This is intentionally NOT automated in the current build. The
`legal_read_article` tool exposes both versions' content; the agent
(or the human lawyer) makes the "more lenient?" call.

---

## Law-name aliases

`src/vault/legal/law_aliases.rs` carries a hand-curated table of ~40
Korean laws with their widely-used short forms:

| Canonical | Short forms |
|---|---|
| 민법 | 민, 民法 |
| 형법 | 형, 刑法 |
| 근로기준법 | 근기법 |
| 근로자퇴직급여 보장법 | 근로자퇴직급여보장법, 근퇴법 |
| 민사소송법 | 민소법, 민소, 민사소송 |
| 형사소송법 | 형소법, 형소, 형사소송 |
| 행정소송법 | 행소법, 행소 |
| 국세기본법 | 국기법 |
| 주택임대차보호법 | 주임법 |
| 독점규제 및 공정거래에 관한 법률 | 공정거래법 |

…and more. Two invariants are compile-time-enforced by tests:

1. No short form maps to two different canonicals (no ambiguity).
2. No canonical appears twice.

Adding a new row is a single line in `LAW_ALIAS_TABLE`. Unknown law
names pass through unchanged — we never invent a slug from a name we
don't recognise.

---

## What's NOT automatic yet (deferred)

These are **known gaps**, not bugs:

### Korean noun extraction for 판결요지 keywords

The original Phase 1 plan listed "판결요지에 포함된 명사 중에서 핵심
키워드" as a keyword source. This requires a Korean morphological
analyser (KoNLPy / mecab-ko / khaiii / …) which isn't currently a
ZeroClaw dependency.

**Workaround in place**: the full 판결요지 text is indexed by FTS5
with a trigram tokenizer (`vault_docs_fts`), so keyword-style search
("반의사불벌죄", "재산분할") works today via
`legal_graph_find` → FTS fallback or direct FTS against the vault.
What's missing is **explicit keyword list extraction** for each 판례.

Adding this would need a Cargo dep for Korean tokenisation and a
nightly job that fills a `vault_tags` row for each extracted noun.
Design note + issue to track this is TODO.

### Historical-version slugs (per-date node splitting)

`구 민법 (2007. 12. 21. 법률 제8720호로 개정되기 전의 것) 제839조의2`
currently canonicalises to the **current** 민법 제839조의2 slug. The
revision-date parenthetical is preserved in `vault_links.context` as
evidence, and the corresponding supplement's `effective_date` +
article's `amendment_dates` let a caller *infer* which version was in
force — but we don't yet split statute articles into per-version
nodes. A future `statute::민법::839-2@20071221` layer would let the
graph carry the full temporal history as distinct nodes; today it's
one node with its history flattened into metadata.

### Computed applicable-version picker

The ingester now captures everything needed to answer "which version
of 근로기준법 제36조 applies to incident of 2024. 3. 28.?":

- `case.incident_date_earliest`
- `statute_article.amendment_dates`
- `statute_supplement.effective_date` joined via `promulgation_date`

but there is no Tool / CLI yet that runs the lookup end-to-end. A
planned `legal_applicable_version(case_slug, statute_slug)` tool will
close the loop for agents.

### 형사사건 경한-개정 예외 자동 판정

Per the exception rule documented above, the "is the new law more
lenient than the old?" judgement is deliberately left to the
agent / human; it requires semantic comparison that deterministic
regex can't cover.

### Ontology / cross-recall integration

Explicitly out of scope:

- Wiring `src/vault/unified_search` into `SqliteMemory::cross_recall`
  (the unified 4-dim vault search is not yet called from the main
  episodic-memory recall pipeline).
- Extending `src/ontology` with an RRF search dimension (schema is
  present, traversal code is not).
- Tauri desktop frontend integration.

Each is a separate follow-up PR.

---

## Source files

- `src/vault/legal/` — extractors, ingest, graph_query, law_aliases, cli, slug, citation_patterns
- `src/tools/vault_graph.rs` — 5 agent tools
- `src/gateway/legal_graph_api.rs` — HTTP endpoints
- `src/gateway/legal_graph_viewer.html` — embedded Cytoscape viewer

All 54 unit tests (as of commit `e401f4e`) live inline in the module
they cover. The user-provided 근로기준법 + 2024노3424 samples serve as
fixtures for the extraction layer.
