# UTF-8 Char-Boundary Truncation Audit — tracking #7828

Working audit trail for the byte-limited string truncation audit requested in
[#7828](https://github.com/zeroclaw-labs/zeroclaw/issues/7828). This is the
**broader recurring-class** follow-up. The scoped Lark / Slack / Twitter /
Notion / Composio / LinkedIn fallback-card sites were already fixed by
[#7455](https://github.com/zeroclaw-labs/zeroclaw/pull/7455) (building on
[#5458](https://github.com/zeroclaw-labs/zeroclaw/pull/5458) and
[#7123](https://github.com/zeroclaw-labs/zeroclaw/pull/7123)); this audit does
**not** reopen or change those PRs. It searches every remaining byte-budget
truncation of runtime-reachable text and classifies each site.

Format follows the excision incident-log convention
(`docs/maintainers/excision-v0.8.0-incidents.md`): per site, a decision and a
one-line reason.

## Decision vocabulary

- **`fixed`** — was a raw byte-budget truncation of user/model/service text
  that could split a multi-byte UTF-8 char; routed through a char-boundary-safe
  path in this audit.
- **`already-fixed`** — prior PR (#5458 / #7455 / #7123) or earlier hardening
  already converted it (inline `is_char_boundary` walk-back, `char_indices()`
  iteration, or `floor_char_boundary`). Verified, not re-touched.
- **`safe-by-invariant`** — the sliced data is hex, base64, binary key/IV
  material, an ASCII-only date/id, or a magic-byte comparison. Cannot split a
  multi-byte char. Invariant stated per row.
- **`safe-by-index-source`** — the index is derived from a scan
  (`str::find`, `char_indices()`, `position`, a `byte_idx` from iteration) and
  is therefore always on a char boundary by construction, independent of any
  byte budget.

## Method

Toolchain: rustc 1.96.1, edition 2024.

Patterns searched (ripgrep, `*.rs`, excluding `**/tests/**` dirs; `#[cfg(test)]`
inline blocks flagged separately and not treated as must-fix):

| id | pattern | raw hits |
|----|---------|----------|
| A | `&<ident>[..<ident\|N>]` string/byte slices | 138 |
| B | `.truncate(...)` | 37 |
| C | `as_bytes()[..]`, `from_utf8(&x[..])` | 7 |
| D | `.get(..N)`, `.split_at(...)` | 11 |

Crates in scope (runtime-reachable text per #7828): `zeroclaw-channels`,
`zeroclaw-tools`, `zeroclaw-providers`, `zeroclaw-runtime`, `zeroclaw-gateway`,
`zeroclaw-memory`, `apps/zerocode`, and the root `src/` binary.

High-risk surfaces reviewed explicitly: channel message chunking/cards
(discord, telegram, lark, slack, matrix, notion, nextcloud), tool output
capping (claude_code, gemini_cli, codex_cli, opencode_cli, google_workspace,
skill_http, skill_tool, shell, screenshot, file_upload, file_download,
content_search, composio), provider error-body scrubbing
(`providers/src/lib.rs`), agent system-prompt + history truncation, memory
consolidation/preview, gateway memory-content API, webhook-audit arg redaction,
and CLI stdin capping.

## Key finding: `str::floor_char_boundary` is now stable

`str::floor_char_boundary` / `str::ceil_char_boundary` are **stable** on the
repo toolchain (verified: a standalone `--edition 2024` probe compiles and runs;
`crates/zeroclaw-runtime/src/tools/skill_manage.rs:254`,
`skills/testing.rs:319`, and `skills/review.rs:265` already call the std method
`s.floor_char_boundary(n)` with no local trait import). The three crate-local
reimplementations —

- `crates/zeroclaw-channels/src/util.rs:13`
- `crates/zeroclaw-tools/src/util_helpers.rs:10`
- `crates/zeroclaw-runtime/src/agent/history.rs:32`

— plus the several inlined `while !s.is_char_boundary(end) { end -= 1 }`
walk-back loops are now functionally redundant with std. Consolidation is
**deferred** (see below), not done here, to keep this audit narrow per #7828's
out-of-scope rule ("no new config or CLI surface"; helper reuse only "where
reuse is justified").

## Findings — sites requiring a fix (`fixed`)

| site | var / source | decision | reason |
|------|--------------|----------|--------|
| `src/main.rs:314` | `line: String` from stdin `read_line`, `.truncate(STDIN_LINE_CAP)` | **fixed** | Only unguarded `String::truncate(byte_cap)` on unvalidated text in the tree. Reachable from the interactive "Press Enter to exit" path when >1 MiB of UTF-8 is piped to `zeroclaw` and byte index `STDIN_LINE_CAP` lands inside a multi-byte char. Reproduced the exact panic (`assertion failed: self.is_char_boundary(new_len)`) with a standalone probe. Sibling `read_capped_line` (`src/main.rs:75`) already caps on the raw `Vec<u8>` before lossy-decoding and is panic-free; this path predates that helper. |

## Findings — representative already-fixed / safe sites

The candidate set is large but overwhelmingly already-safe. Representative rows
(full list is mechanical — every `&s[..N]`/`.truncate(N)` of text either matches
one of these shapes or is byte/ASCII data):

| site | decision | reason |
|------|----------|--------|
| `zeroclaw-tools/src/claude_code.rs:269`, `gemini_cli.rs:194`, `codex_cli.rs:204`, `opencode_cli.rs:189`, `google_workspace.rs:454/462`, `runtime/src/tools/skill_tool.rs:299/307`, `skill_http.rs:160`, `runtime/src/tools/shell.rs:564` | already-fixed | tool stdout/stderr capping; each does the `while !s.is_char_boundary(b) { b -= 1 }` walk-back before `truncate(b)`. |
| `runtime/src/agent/system_prompt.rs:413` | already-fixed | system-prompt budget truncation; boundary walk-back before `prompt.truncate(end)`. |
| `runtime/src/rpc/dispatch.rs:4064` | already-fixed | memory-preview content; boundary walk-back. |
| `gateway/src/api.rs:1001` (`truncate_with_ellipsis_total_chars`) | already-fixed | uses `char_indices().nth()` for the cut index. |
| `runtime/src/security/external_content.rs:179`, `channels/src/discord/mod.rs:3622`, `channels/src/telegram.rs:3403`, `channels/src/lark.rs:467`, `memory/src/consolidation.rs:86/242`, `runtime/src/skills/creator.rs:384` | already-fixed | `char_indices()` accumulation of `next = idx + ch.len_utf8()` capped at the byte budget — never splits a char. |
| `channels/src/notion.rs:315/441/579`, `slack.rs:4317/4334`, `twitter.rs:450`, `git/channel.rs:621`, `lark.rs:3937`, `tools/src/composio.rs:1100/1189`, `tools/src/linkedin_client.rs:1189`, `providers/src/lib.rs:882`, `tools/src/file_upload.rs:146`, `file_download.rs:338`, `file_upload_bundle.rs:59`, `content_search.rs:1051`, `apps/zerocode/src/chat.rs:3199` | already-fixed | route through `floor_char_boundary` (crate-local or std) or an inlined equivalent. |
| `runtime/src/tools/skill_manage.rs:254`, `skills/testing.rs:319`, `skills/review.rs:265` | already-fixed | use **std** `str::floor_char_boundary`. |
| `nextcloud_talk.rs:638`, `matrix.rs:1291` | already-fixed | `char_indices().nth(limit)` for the cut. |
| `channels/src/email_channel.rs:441/462`, `runtime/src/rpc/attachments.rs:136`, `gateway/src/lib.rs:7181` | safe-by-invariant | slice hex output (`hex::encode(&digest[..16])`, `&hex[..16]`, `&hex_sig[..32]`) — hex is ASCII, 1 byte/char. |
| `channels/src/wecom_ws.rs:270/271` | safe-by-invariant | `&raw_key[..32]` / `&key[..16]` are length-checked binary AES key/IV bytes, not text. |
| `channels/src/wecom_ws.rs:2128` | safe-by-invariant | `&bytes[..4] == b"RIFF"` magic-byte comparison on `&[u8]`. |
| `channels/src/orchestrator/mod.rs:25215/25240` | safe-by-invariant | `&date_line[..10]` in a test; slices an ASCII `YYYY-MM-DD` date. |
| `channels/src/link_enricher.rs:193` | safe-by-invariant | `from_utf8_lossy(&bytes[..max_bytes])` slices a `&[u8]` then lossy-decodes — a split tail codepoint becomes U+FFFD, no panic. |
| `channels/src/screenshot.rs:180` | safe-by-invariant | base64 output is ASCII (and it still walks the boundary defensively). |
| `zeroclaw-tool-call-parser/src/lib.rs` (`&line[..pos]`, `&after_quote[..end_quote]`, `&remaining[..start]`, `&after_open[..close_idx]`, `&cleaned_text[..start]`, …), `providers/src/{gemini,bedrock,multimodal,ollama,compatible,lib}.rs` (`&rest[..semi]`, `&source[..comma_idx]`, `&rest[..start]`, `&raw_name[..idx]`), `channels/src/{irc,matrix,amqp,imessage,slack,discord/markers,orchestrator}.rs`, `runtime/src/{agent/*,sop,skills/audit,tunnel/*}.rs`, `config/src/{policy,helpers}.rs`, `apps/zerocode/src/{chat,help}.rs` | safe-by-index-source | index comes from `str::find` / `char_indices()` / a scanned `byte_idx`, so it is always on a char boundary regardless of any byte budget. |
| `providers/src/{openai_codex,compatible}.rs`, `config/src/schema.rs:10037/10038`, `hardware/src/*`, `channels/src/filesystem.rs`, `runtime/src/skills/{cache,mod}.rs`, `plugin_registry.rs`, `web_fetch.rs`, `stream_guard.rs`, buffer `.extend_from_slice(&chunk[..n])` sites | safe-by-invariant | slice `&[u8]` buffers (I/O reads, `from_utf8`/`from_utf8_lossy` on byte slices, `valid_up_to()`), not `&str`; no char-boundary contract applies. |

## Deferred / out of scope

- **Helper consolidation onto std `floor_char_boundary`.** With
  `str::floor_char_boundary` stable, the three crate-local copies and the
  inlined walk-back loops are redundant. Collapsing them touches
  `zeroclaw-channels`, `zeroclaw-tools`, and `zeroclaw-runtime` public/`pub(crate)`
  surfaces and every call site — a multi-crate refactor whose risk/benefit does
  not fit #7828's "narrow audit, no API changes" mandate (compare the excision
  log's deferral of cross-crate consolidation as "risky on a release branch").
  Recommend a dedicated follow-up issue: replace crate-local `floor_char_boundary`
  with `str::floor_char_boundary` and delete the reimplementations + their tests.
- **Changing channel/provider/tool length limits** — explicitly out of scope; no
  limit was changed.
- **Test-only slices** (`browser.rs:1942 &script[..60]`, `matrix.rs:5548/5572`,
  `orchestrator/mod.rs:25215/25240`) — slice static ASCII fixtures in
  `#[cfg(test)]`; left as-is.

## Regression coverage

Per #7828, the one `fixed` path (`src/main.rs:314`) gets a focused regression
test using neutral multi-byte placeholder text whose byte cap lands inside a
character, asserting no panic and `result.is_char_boundary(result.len())`. Added
in the same change as the fix. (Adding new tests is the sanctioned deviation
from the repo's "don't touch tests" default, since acceptance criterion 3
requires multi-byte regression coverage for changed paths.)

## Acceptance-criteria mapping

1. *Records searched patterns + reviewed high-risk surfaces* → Method table +
   High-risk surfaces list.
2. *Every remaining byte-index text slice fixed or has a clear invariant* →
   Findings tables (`fixed` / `already-fixed` / `safe-by-invariant` /
   `safe-by-index-source`).
3. *Fixed paths have multi-byte regression coverage* → Regression coverage.
4. *Public text stays clear that #7455 fixed its scope* → header scope note.

## Change log

- 2026-07-08: Initial audit. One straggler found and fixed (`src/main.rs:314`);
  all other runtime-reachable truncation sites classified as already-fixed or
  provably safe. Documented that `str::floor_char_boundary` is now stable and
  recommended a follow-up helper-consolidation issue.
