# Mass-Close Audit: 2026-04-12

**Author**: @theonlyhennygod
**Action**: Closed 72 issues between 19:16–19:27 UTC (11 minutes)
**Comment on all**: "Closing — addressed by an existing PR." (no specific PR linked)

This document tracks whether each closed issue actually has a matching PR.

## Legend

- **MATCHED_MERGED** — A merged PR plausibly addresses this issue
- **MATCHED_OPEN** — An open PR exists but hasn't merged yet (issue should NOT have been closed)
- **NO_MATCH** — No PR found that addresses this issue (issue should NOT have been closed)
- **UNCLEAR** — Tangentially related PR exists but doesn't clearly resolve the issue

## Summary

| Status | Count |
|---|---|
| MATCHED_MERGED | 9 |
| MATCHED_OPEN | 39 |
| NO_MATCH | 23 |
| UNCLEAR | 1 |
| **Total** | **72** |

**Issues that should be reopened: ~63** (39 MATCHED_OPEN + 23 NO_MATCH + 1 UNCLEAR).
Only the 9 MATCHED_MERGED issues were arguably correct to close, though even those lacked proper attribution.

---

## Full Audit

### MATCHED_MERGED (9) — Potentially valid closures

| Issue | Title | PR | Notes |
|---|---|---|---|
| 4868 | allowed_private_hosts config for SSRF bypass | #4590 | Merged |
| 5221 | Model cost not captured for schedules, command line and web agents | #5484 | Merged; also #5302 open |
| 5268 | Context compressor drops tool_call_id from trimmed messages | #5457 | Merged; directly fixes this |
| 5299 | Installer aborts on empty cargo feature args under set -u | #5666 | Merged; install.sh rewritten |
| 5348 | Web dashboard not available | #5675 | Merged; includes dashboard in binary releases |
| 5445 | config.toml forward-only schema versioning and V1→V2 migration | #5517 | Still open — not yet merged |
| 5465 | Failed to config workspace root (Windows fsync) | #5296 | Merged |
| 5651 | install.sh update for workspace-split v0.6.9 | #5666 | Merged |
| 5655 | add enabled field for Email and VoiceCall | #5659 | Merged; issue was tracking for this PR |

**Note**: #5445 is listed here but PR #5517 is still open — this issue was closed prematurely.

### MATCHED_OPEN (39) — Should be reopened (PR exists but not merged)

| Issue | Title | PR | Notes |
|---|---|---|---|
| 4830 | HMAC tool execution receipts | #5168 | Open; earlier #4831 and #4943 closed |
| 4832 | Disable LeakDetector high-entropy token redaction | #5080 | Open |
| 4842 | update command wrong arch on aarch64 | #5086 | Open |
| 4846 | WhatsApp-Web Channel Broken | #5099 | Open |
| 4848 | MCP's not working | #5100 | Open |
| 4851 | configure GitHub Copilot as provider | #5321 | Open; also #5098 |
| 4853 | Installing skills from .well-known URI | #5101 | Open |
| 4873 | Feishu: only LLM called, not Agent | #5111 | Open |
| 4878 | E2EE recovery never downloads room keys | #5097 | Open |
| 4879 | Gemini CLI OAuth not working | #5106 | Open; also #5314 |
| 4880 | context_compression not triggered in daemon mode | #5085 | Open |
| 4896 | Anthropic-compatible endpoints in onboarding | #5105 | Open |
| 4916 | auto_save recursive snowball | #4936 | Open; #5664 merged for cron subset |
| 4955 | Hardcoded third-party repo for open-skills | #5103 | Open |
| 5122 | allowed_private_hosts useless for DNS | #5136 | Open |
| 5144 | Matrix failed to decrypt room event | #5150 | Open; related not exact |
| 5145 | add send_channel_message tool | #5152 | Open |
| 5183 | Slack env var authentication | #5310 | Open |
| 5244 | Dashboard Channels tab crash | #5375 | Open |
| 5253 | Add musl build in release page | #5660 | Open |
| 5285 | Thoughts merge into final message GLM-5 | #5298 | Open |
| 5360 | codex_cli passes unsupported -q flag | #5361 | Open |
| 5470 | Multiple issues when running safely | #5481 | Open |
| 5475 | Copilot + Telegram Invalid parameter | #5481 | Open |
| 5500 | Ollama hardcodes supports_native_tools() = false | #5523 | Open |
| 5518 | forbidden_path_argument blocks safe redirects | #5524 | Open |
| 5527 | Gemini changed OAuth things again | #5539 | Open |
| 5533 | allowed_Path doesn't respect contains logic | #5546 | Open |
| 5536 | Embedding search results score display bug | #5671 | Open |
| 5537 | Causes Persistent Error Loop | #5549 | Open |
| 5541 | Dockerfile.debian three bugs | #5545 | Open |
| 5542 | consecutive OOM in wsl2 | #5548 | Open |
| 5550 | autosaved memories invisible to recall | #5632 | Open; also #5631 |
| 5562 | Windows shell commands flash console | #5563 | Open |
| 5564 | Custom provider tool follow-up fails on empty output | #5565 | Open |
| 5583 | Docker.debian image fails to build | #5592 | Open; also #5545 |
| 5604 | Mattermost private messages | #5602 | Open |
| 5617 | Phase 2 D5: Reduce all_tools_with_runtime | #5566 | Open |
| 5619 | Native OpenRouter provider routing support | #5623 | Open; #5621 closed |
| 5629 | api_key falsely warned as unknown config key | #5673 | Open |
| 5634 | Web dashboard creates new session on every page load | #5641 | Open |
| 5654 | encryption for telegrom token not working | #5669 | Open |
| 5670 | Groq provider 400 error | #5676 | Open |
| 5672 | Feishu responds even when mention_only enabled | #5676 | Open |

### NO_MATCH (23) — Should be reopened (no PR exists)

| Issue | Title | Notes |
|---|---|---|
| 4710 | A better LOGO of Zeroclaw | Design request; no PR |
| 4866 | Web dashboard is still not available | #5365 tangential (packaging); core complaint unresolved |
| 5318 | stream_mode Partial: hide thinking content | Feature request; no PR |
| 5356 | Canvas tool writes to separate CanvasStore | No PR addresses this |
| 5447 | Crate split the crate | Feature request; workspace split happened but no dedicated PR |
| 5501 | Trigger cron manually | Feature request; no PR |
| 5502 | Add allowed_tools configuration to AgentConfig | No new PR matches |
| 5509 | Telegram voice message transcription | No PR for Telegram voice specifically |
| 5528 | Improper logic of email channel config | No direct fix PR |
| 5556 | Summarization timed out after 60s | No PR found |
| 5558 | Feishu ack_reactions=false has no effect | #5676 fixes mention_only but not ack_reactions |
| 5570 | Faster SQLite memory vector search (ANN) | Enhancement; no PR |
| 5575 | Extremely slow project compilation | No direct PR |
| 5578 | Zeroclaw doesn't talk to local llama.cpp server | No matching PR |
| 5584 | Duplicate assistant messages with narration + tool calls | No PR found |
| 5586 | Phase 1 D4: WIT interface files | Deferred; no PR created |
| 5600 | kimi-code provider streaming error | No PR found |
| 5605 | Default Configuration Path Issues Multi-Instance | No PR found |
| 5649 | Clipboard paste & drag-and-drop in Web Chat UI | No PR |
| 5656 | refactor(hardware): move wizard UI | No PR found |

### UNCLEAR (1)

| Issue | Title | PR | Notes |
|---|---|---|---|
| 4866 | Web dashboard is still not available | #5365 | Packaging-related, not the core availability complaint |

---

## Recommended Actions

1. **Reopen all 63 non-MATCHED_MERGED issues** with a comment explaining the mass-close was premature
2. **For the 39 MATCHED_OPEN issues**: reopen and link to the relevant open PR
3. **For the 23 NO_MATCH issues**: reopen with no change
4. **For the 9 MATCHED_MERGED issues**: verify the merged PR actually resolves the issue; reopen if not
5. **Review theonlyhennygod's permissions** — closing 72 issues in 11 minutes with no triage is not legitimate issue management
6. **Reopen #5445 specifically** — PR #5517 is still open, issue was closed prematurely
