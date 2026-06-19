# String Truncation Safety Audit

## Date: 2026-06-19
## Scope: All Rust source files in workspace
## Classification per issue: FILE_READ_INDEX_OUT_OF_BOUNDS

This audit identifies all locations where `String::truncate()` or `&s[..N]` / `&s[..some_computed_idx]` is used to truncate a UTF-8 string, and verifies that the truncation point falls on a valid UTF-8 character boundary.

---

## PATTERN CLASSES

### 1. `String::truncate(usize)` — safe only when index is a char boundary

| File | Line | Safe? | Notes |
|---|---|---|---|
| `crates/zeroclaw-runtime/src/agent/history.rs` | ~138 | ✅ | Uses `floor_char_boundary` |
| `crates/zeroclaw-memory/src/driver.rs` | various | ✅ | `Vec::truncate` (not String) |
| `crates/zeroclaw-memory/src/agent_scoped_markdown.rs` | ~160 | ✅ | `Vec::truncate` (not String) |

### 2. `String::drain(..idx)` — safe only when idx is a char boundary

| File | Line | Safe? | Notes |
|---|---|---|---|
| `crates/zeroclaw-providers/src/compatible.rs` | 1549, 1652 | ✅ | `drain(..=pos)` where `pos` is from `.find('\n')` — ASCII, always a char boundary |

### 3. `&s[..idx]` or `&s[..N]` — substring slice

See the full table below.

---

## COMPREHENSIVE FINDINGS

### ✅ SAFE — uses `floor_char_boundary` / `is_char_boundary` guard / `char_indices()`

Every instance in this section is either:
- Preceded by a `while !s.is_char_boundary(idx)` loop
- Uses `floor_char_boundary()`
- Uses `char_indices().nth(N)` to get the byte index
- Truncates at a position found by `.find()` for an ASCII/known-byte-width pattern

| File | Line(s) | Guard/Pattern |
|---|---|---|
| `zeroclaw-gateway/src/api.rs` | 1015 | `char_indices().nth(cut_idx)` |
| `zeroclaw-runtime/src/agent/system_prompt.rs` | 403 | `while !prompt.is_char_boundary(end)` |
| `zeroclaw-runtime/src/rpc/dispatch.rs` | 3522 | `while !entry.content.is_char_boundary(end)` |
| `zeroclaw-providers/src/lib.rs` | 873 | `char_indices().nth()` |
| `zeroclaw-gateway/src/api_personality.rs` | 146 | `char_indices().nth(max)` |
| `zeroclaw-runtime/src/agent/personality.rs` | 142 | `char_indices().nth()` |
| `zeroclaw-runtime/src/agent/history.rs` | 139 | `floor_char_boundary` |
| `zeroclaw-runtime/src/agent/turn/redact.rs` | 35 | `char_indices().nth(4)` |
| `zeroclaw-channels/src/util.rs` | 5 | `char_indices().nth()` |
| `zeroclaw-channels/src/nextcloud_talk.rs` | 634 | `char_indices().nth()` |
| `zeroclaw-channels/src/lark.rs` | 3566 | `floor_char_boundary` |
| `zeroclaw-channels/src/notion.rs` | 305, 431, 558 | `floor_char_boundary` |
| `zeroclaw-channels/src/util.rs` | 5 | `char_indices().nth()` |
| `zeroclaw-channels/src/discord/mod.rs` | 493 | truncates at `find(" [edited at ")` which is ASCII |
| `zeroclaw-channels/src/link_enricher.rs` | 193 | `String::from_utf8_lossy(&bytes[..max_bytes])` — bytes not str |
| `zeroclaw-tools/src/file_upload.rs` | 146 | `floor_char_boundary` |
| `zeroclaw-tools/src/file_upload_bundle.rs` | 59 | `char_indices().nth()` |
| `zeroclaw-tools/src/composio.rs` | 1100, 1189 | `floor_char_boundary` |
| `zeroclaw-tools/src/linkedin_client.rs` | 1189 | `floor_char_boundary` |
| `zeroclaw-tools/src/content_search.rs` | 630 | `char_indices().nth()` |
| `zeroclaw-tools/src/skill_manage.rs` | 257 | `floor_char_boundary` |
| `zeroclaw-runtime/src/daemon/mod.rs` | 997 | `char_indices().nth()` |
| `zeroclaw-runtime/src/util.rs` | 58 | `char_indices().nth()` |
| `zeroclaw-runtime/src/hooks/builtin/webhook_audit.rs` | 246 | `floor_char_boundary` |
| `zeroclaw-runtime/src/cron/store.rs` | 720 | `while !is_char_boundary` guard |
| `zeroclaw-runtime/src/heartbeat/store.rs` | 191 | `while !is_char_boundary` guard |
| `zeroclaw-runtime/src/agent/loop_.rs` | 3190 | Uses `.find("... truncated ...")` — safe |
| `zeroclaw-runtime/src/skills/audit.rs` | 418 | `&content[..128]` — content is `Vec<u8>` (bytes), not String/str |
| `zeroclaw-runtime/src/skills/audit.rs` | 486 | End from `.find('#')` / `.find('?')` which are ASCII |
| `zeroclaw-runtime/src/skills/review.rs` | 246 | `floor_char_boundary` |
| `zeroclaw-runtime/src/skills/creator.rs` | 120 | `char_indices().nth()` |
| `zeroclaw-runtime/src/skills/document.rs` | 67 | `rest.find("\n---\n")` — all ASCII |
| `zeroclaw-runtime/src/skills/mod.rs` | 1247 | `rest.find("\n---\n")` — all ASCII |
| `zeroclaw-runtime/src/skills/improver.rs` | 188, 325 | `.find()` — ASCII delimiters |
| `zeroclaw-providers/src/bedrock.rs` | 1105, 1114, 1121 | `find()` — ASCII patterns |
| `zeroclaw-providers/src/ollama.rs` | 335 | `find("<think>")` — ASCII |
| `zeroclaw-providers/src/compatible.rs` | 892 | `find("<think>")` — ASCII |
| `zeroclaw-providers/src/multimodal.rs` | 970 | `find(',')` — ASCII |
| `zeroclaw-providers/src/openai.rs` | 819 | `find("\n\n")` — ASCII |
| `zeroclaw-providers/src/openai_codex.rs` | 893 | `find("\n\n")` — ASCII |
| `zeroclaw-channels/src/telegram.rs` | 121, 258, 262, 268, 3279 | Uses `byte_index_after_chars()` or `char_indices()` based calcs |
| `zeroclaw-channels/src/discord/mod.rs` | 2594 | `char_indices().nth()` |
| `zeroclaw-channels/src/discord/chunk.rs` | 28, 33, 47 | `.chars().count()` / `.find('\n')` — safe |
| `zeroclaw-channels/src/matrix.rs` | 95, 99, 104, 3440 | `.find()` — ASCII patterns |
| `zeroclaw-channels/src/line.rs` | 825, 827 | `.rfind('\n')` / `.rfind(' ')` — ASCII |
| `zeroclaw-channels/src/slack.rs` | 4016–4030 | `.rfind('\n')` / `.rfind(' ')` + floor_char_boundary |
| `zeroclaw-channels/src/twitter.rs` | 439–440 | `floor_char_boundary` |
| `zeroclaw-channels/src/whatsapp_web.rs` | 1120 | `text[..pos].chars()` — iteration over chars, safe |
| `zeroclaw-channels/src/irc.rs` | 82, 89, 116, 222 | `.find()` — ASCII |
| `zeroclaw-channels/src/imessage.rs` | 149 | `find('@')` — ASCII |
| `zeroclaw-channels/src/wecom_ws.rs` | 1802, 2355 | byte arithmetic on bytes, tested |
| `zeroclaw-channels/src/amqp.rs` | 366, 369 | `.find('{')` / `find('}')` — ASCII |
| `zeroclaw-channels/src/notion.rs` | 305, 431, 557–558 | `floor_char_boundary` |
| `zeroclaw-channels/src/orchestrator/mod.rs` | 725, 971, 1597, 1612, 1752, 2657, 2692, 3018, 3340, 3367 | All used with `.find()` for ASCII patterns or `nth()` char-based |
| `zeroclaw-channels/src/signal.rs` | 553 | `.find('\n')` — ASCII |
| `zeroclaw-runtime/src/agent/turn/stream_guard.rs` | 55, 247, 256 | `.find()` — ASCII tags |
| `zeroclaw-runtime/src/agent/dispatcher.rs` | 43, 103 | `.find()` — ASCII tags |
| `zeroclaw-runtime/src/agent/loop_.rs` | 171 | `.find('*')` — ASCII |
| `zeroclaw-runtime/src/agent/thinking.rs` | 50 | `.find('>')` — ASCII |
| `zeroclaw-runtime/src/agent/turn/protocol_detect.rs` | 14 | `text.ends_with(&pattern[..len])` — iterated, safe |
| `zeroclaw-runtime/src/tools/shell.rs` | 505 | `&chunk[..take]` — `chunk` is `&[u8]`, not a str |
| `zeroclaw-runtime/src/tools/file_read.rs` | 441 | `&bytes[..8192]` — `bytes` is `&[u8]`, not a str |
| `zeroclaw-runtime/src/tools/delegate.rs` | 2687, 2691 | `&chunk[..n]` + `&buf[..header_end]` — bytes |
| `zeroclaw-runtime/src/tunnel/cloudflare.rs` | 17 | `.find('/')` — ASCII |
| `zeroclaw-runtime/src/tunnel/custom.rs` | 95, 102 | `.find('/')` — ASCII |
| `zeroclaw-runtime/src/tunnel/ngrok.rs` | 95 | `.find('/')` — ASCII |
| `zeroclaw-runtime/src/tunnel/pinggy.rs` | 139 | `.find('/')` — ASCII |
| `zeroclaw-runtime/src/sop/mod.rs` | 342 | `.find(". ")` — ASCII |
| `zeroclaw-runtime/src/sop/condition.rs` | 90 | `.find()` — ASCII operators |
| `zeroclaw-runtime/src/skills/improver.rs` | 368, 370 | `.find('.')` — ASCII |
| `zeroclaw-runtime/src/skills/audit.rs` | 418, 486 | Bytes or `.find('#')` — safe |
| `zeroclaw-runtime/src/skills/review.rs` | 269 | `.find('\'')` — ASCII |
| `zeroclaw-runtime/src/daemon/mod.rs` | 997 | Already verified — uses `char_indices()` |
| `zeroclaw-config/src/policy.rs` | 1418, 1532 | `.find(['<', '>'])` — ASCII |
| `zeroclaw-config/src/helpers.rs` | 49, 168 | Exact-key match — valid boundary |
| `zeroclaw-tool-call-parser/src/lib.rs` | 478, 485, 603, 613, 1241, 1322, 1329, 1414, 1428, 1438, 1461, 1470, 1500, 1610, 1630, 1673, 1712, 1851, 1881, 1911, 1974 | All `.find()` with ASCII delimiters or regex-split |
| `zeroclaw-memory/src/consolidation.rs` | 79, 170 | `char_indices().nth()` |
| `zeroclaw-memory/src/hygiene.rs` | 437 | `filename[..boundary]` — filename is `&str`, boundary from `.rfind('-')` (ASCII) |
| `plugins/languagetool/src/lib.rs` | 122 | `char_indices().nth()` |
| `plugins/sd-webui/src/lib.rs` | 115 | `char_indices().nth()` |
| `plugins/image-gen-fal/src/lib.rs` | 208 | `char_indices().nth()` |
| `apps/zerocode/src/chat.rs` | 2422, 2466, 2842 | `char_indices().nth()` or `.find(',')` — safe |
| `apps/zerocode/src/input_bar.rs` | 760, 774 | `.graphemes(true)` on slice |
| `apps/zerocode/src/logs.rs` | 143 | `end.min(12)` — but `end` already computed from `.nth()`, min is just guard |
| `xtask/` | various | All parsing code with `.find()` on ASCII patterns — safe |
| `firmware/` | various | Byte arrays, not strings |
| `tests/` | various | Content slices, safe |

### ✅ SAFE — Non-string truncation (Vec, bytes, etc.)

| File | Line | Notes |
|---|---|---|
| `crates/zeroclaw-memory/src/driver.rs` | 138 | `Vec::truncate` |
| `crates/zeroclaw-memory/src/agent_scoped_markdown.rs` | 160 | `Vec::truncate` |
| `crates/aardvark-sys/src/lib.rs` | 215, 220 | Slice on `Vec<u16>` |
| `crates/zeroclaw-hardware/src/subprocess.rs` | 353 | `&buf[..n]` — bytes via `from_utf8_lossy` |
| `crates/zeroclaw-hardware/src/peripherals/uno_q_bridge.rs` | 55 | `&buf[..n]` — bytes via `from_utf8_lossy` |

### ❌ POTENTIALLY UNSAFE

| File | Line | Code | Risk |
|---|---|---|---|
| `crates/zeroclaw-runtime/src/skills/testing.rs` | 303 | `&trimmed[..max]` where `max` is a byte count from caller, **no char boundary guard** | **BUG**: If `trimmed` contains multi-byte UTF-8 chars and `max` falls in the middle of one, this panics. Also, line 300 compares `trimmed.len()` (byte count) to `max`, so equality may be misleading for non-ASCII content. |

The `truncate_output` function at `crates/zeroclaw-runtime/src/skills/testing.rs:298` is called with `max=200` at line 270 (in the test failure reporting path). While this is currently only used for display in CLI test output (not safety-critical), it will panic on any skill output containing multi-byte Unicode characters whose byte length exceeds 200 at a non-boundary position.

---

## SUMMARY

- **Total locations examined**: ~200
- **Safe**: ~199
- **Unsafe**: 1 (`crates/zeroclaw-runtime/src/skills/testing.rs:298–303` — `truncate_output` function)
- **Immediate risk**: Low (test CLI display path, exercised only on skill test failure with non-ASCII output)
- **Fix needed**: Add `floor_char_boundary` or `char_indices` guard before slicing
