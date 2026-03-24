# Label Registry

Single reference for every label used on PRs and issues. Labels are grouped by category. Each entry lists the label name, definition, and how it is applied.

Sources consolidated here:

- `.github/labeler.yml` (path-label config for `actions/labeler`)
- `.github/label-policy.json` (contributor tier thresholds)
- `docs/contributing/pr-workflow.md` (size, risk, and triage label definitions)
- `docs/contributing/ci-map.md` (automation behavior and high-risk path heuristics)

---

## Path labels

Applied automatically by `pr-path-labeler.yml` using `actions/labeler`. Matches changed files against glob patterns in `.github/labeler.yml`.

### Base scope labels

| Label | Matches |
|---|---|
| `docs` | `docs/**`, `**/*.md`, `**/*.mdx`, `LICENSE`, `.markdownlint-cli2.yaml` |
| `dependencies` | `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.github/dependabot.yml` |
| `ci` | `.github/**`, `.githooks/**` |
| `core` | `src/*.rs` |
| `agent` | `src/agent/**` |
| `channel` | `src/channels/**` |
| `gateway` | `src/gateway/**` |
| `config` | `src/config/**` |
| `cron` | `src/cron/**` |
| `daemon` | `src/daemon/**` |
| `doctor` | `src/doctor/**` |
| `health` | `src/health/**` |
| `heartbeat` | `src/heartbeat/**` |
| `integration` | `src/integrations/**` |
| `memory` | `src/memory/**` |
| `security` | `src/security/**` |
| `runtime` | `src/runtime/**` |
| `onboard` | `src/onboard/**` |
| `provider` | `src/providers/**` |
| `service` | `src/service/**` |
| `skillforge` | `src/skillforge/**` |
| `skills` | `src/skills/**` |
| `tool` | `src/tools/**` |
| `tunnel` | `src/tunnel/**` |
| `observability` | `src/observability/**` |
| `tests` | `tests/**` |
| `scripts` | `scripts/**` |
| `dev` | `dev/**` |

### Per-component channel labels

Each channel gets a specific label in addition to the base `channel` label.

| Label | Matches |
|---|---|
| `channel:bluesky` | `bluesky.rs` |
| `channel:clawdtalk` | `clawdtalk.rs` |
| `channel:cli` | `cli.rs` |
| `channel:dingtalk` | `dingtalk.rs` |
| `channel:discord` | `discord.rs`, `discord_history.rs` |
| `channel:email` | `email_channel.rs`, `gmail_push.rs` |
| `channel:imessage` | `imessage.rs` |
| `channel:irc` | `irc.rs` |
| `channel:lark` | `lark.rs` |
| `channel:linq` | `linq.rs` |
| `channel:matrix` | `matrix.rs` |
| `channel:mattermost` | `mattermost.rs` |
| `channel:mochat` | `mochat.rs` |
| `channel:mqtt` | `mqtt.rs` |
| `channel:nextcloud-talk` | `nextcloud_talk.rs` |
| `channel:nostr` | `nostr.rs` |
| `channel:notion` | `notion.rs` |
| `channel:qq` | `qq.rs` |
| `channel:reddit` | `reddit.rs` |
| `channel:signal` | `signal.rs` |
| `channel:slack` | `slack.rs` |
| `channel:telegram` | `telegram.rs` |
| `channel:twitter` | `twitter.rs` |
| `channel:wati` | `wati.rs` |
| `channel:webhook` | `webhook.rs` |
| `channel:wecom` | `wecom.rs` |
| `channel:whatsapp` | `whatsapp.rs`, `whatsapp_storage.rs`, `whatsapp_web.rs` |

### Per-component provider labels

| Label | Matches |
|---|---|
| `provider:anthropic` | `anthropic.rs` |
| `provider:azure-openai` | `azure_openai.rs` |
| `provider:bedrock` | `bedrock.rs` |
| `provider:claude-code` | `claude_code.rs` |
| `provider:compatible` | `compatible.rs` |
| `provider:copilot` | `copilot.rs` |
| `provider:gemini` | `gemini.rs`, `gemini_cli.rs` |
| `provider:glm` | `glm.rs` |
| `provider:kilocli` | `kilocli.rs` |
| `provider:ollama` | `ollama.rs` |
| `provider:openai` | `openai.rs`, `openai_codex.rs` |
| `provider:openrouter` | `openrouter.rs` |
| `provider:telnyx` | `telnyx.rs` |

### Per-group tool labels

Tools are grouped by logical function rather than one label per file.

| Label | Matches |
|---|---|
| `tool:browser` | `browser.rs`, `browser_delegate.rs`, `browser_open.rs`, `text_browser.rs`, `screenshot.rs` |
| `tool:cloud` | `cloud_ops.rs`, `cloud_patterns.rs` |
| `tool:composio` | `composio.rs` |
| `tool:cron` | `cron_add.rs`, `cron_list.rs`, `cron_remove.rs`, `cron_run.rs`, `cron_runs.rs`, `cron_update.rs` |
| `tool:file` | `file_edit.rs`, `file_read.rs`, `file_write.rs`, `glob_search.rs`, `content_search.rs` |
| `tool:google-workspace` | `google_workspace.rs` |
| `tool:mcp` | `mcp_client.rs`, `mcp_deferred.rs`, `mcp_protocol.rs`, `mcp_tool.rs`, `mcp_transport.rs` |
| `tool:memory` | `memory_forget.rs`, `memory_recall.rs`, `memory_store.rs` |
| `tool:microsoft365` | `microsoft365/**` |
| `tool:security` | `security_ops.rs`, `verifiable_intent.rs` |
| `tool:shell` | `shell.rs`, `node_tool.rs`, `cli_discovery.rs` |
| `tool:sop` | `sop_advance.rs`, `sop_approve.rs`, `sop_execute.rs`, `sop_list.rs`, `sop_status.rs` |
| `tool:web` | `web_fetch.rs`, `web_search_tool.rs`, `web_search_provider_routing.rs`, `http_request.rs` |

---

## Size labels

Defined in `pr-workflow.md` §6.1. Based on effective changed line count, normalized for docs-only and lockfile-heavy PRs.

| Label | Threshold |
|---|---|
| `size: XS` | <= 80 lines |
| `size: S` | <= 250 lines |
| `size: M` | <= 500 lines |
| `size: L` | <= 1000 lines |
| `size: XL` | > 1000 lines |

**Applied by:** `pr-labeler.yml` (not yet implemented). Requires custom logic because `actions/labeler` cannot count changed lines.

---

## Risk labels

Defined in `pr-workflow.md` §13.2 and `ci-map.md`. Based on a heuristic combining touched paths and change size.

| Label | Meaning |
|---|---|
| `risk: low` | No high-risk paths touched, small change |
| `risk: medium` | Behavioral `src/**` changes without boundary/security impact |
| `risk: high` | Touches high-risk paths (see below) or large security-adjacent change |
| `risk: manual` | Maintainer override that freezes automated risk recalculation |

High-risk paths: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`.

The boundary between low and medium is not formally defined beyond "no high-risk paths."

**Applied by:** `pr-labeler.yml` (not yet implemented). Auto-corrected on manual label edits. `risk: manual` exempts a PR from automated recalculation.

---

## Contributor tier labels

Defined in `.github/label-policy.json`. Based on the author's merged PR count queried from the GitHub API.

| Label | Minimum merged PRs |
|---|---|
| `trusted contributor` | 5 |
| `experienced contributor` | 10 |
| `principal contributor` | 20 |
| `distinguished contributor` | 50 |

**Applied by:** `pr-labeler.yml` on PRs, `pr-auto-response.yml` on issues (neither yet implemented). Automation-managed; manual add/remove is auto-corrected.

---

## Response and triage labels

Defined in `pr-workflow.md` §8. Applied manually or by triage automation.

| Label | Purpose | Applied by |
|---|---|---|
| `r:needs-repro` | Incomplete bug report; request deterministic repro | Manual or `pr-auto-response.yml` |
| `r:support` | Usage/help item better handled outside bug backlog | Manual or `pr-auto-response.yml` |
| `invalid` | Not a valid bug/feature request | Manual |
| `duplicate` | Duplicate of existing issue | Manual |
| `stale-candidate` | Dormant PR/issue; candidate for closing | Manual or `pr-check-stale.yml` |
| `superseded` | Replaced by a newer PR | Manual |
| `no-stale` | Exempt from stale automation; accepted but blocked work | Manual |

**Automation:** `invalid` and `duplicate` trigger issue-only closing automation via `pr-auto-response.yml`. PRs are never auto-closed by route labels.

---

## Implementation status

| Category | Count | Automated | Workflow |
|---|---|---|---|
| Path (base scope) | 27 | Yes | `pr-path-labeler.yml` |
| Path (per-component) | 52 | Yes | `pr-path-labeler.yml` |
| Size | 5 | No | `pr-labeler.yml` (not yet implemented) |
| Risk | 4 | No | `pr-labeler.yml` (not yet implemented) |
| Contributor tier | 4 | No | `pr-labeler.yml` / `pr-auto-response.yml` (not yet implemented) |
| Response/triage | 7 | Partial | Manual + `pr-auto-response.yml` / `pr-check-stale.yml` (not yet implemented) |
| **Total** | **99** | | |

---

## Maintenance

- **Owner:** maintainers responsible for label policy and PR triage automation.
- **Update trigger:** new channels, providers, or tools added to the source tree; label policy changes; triage workflow changes.
- **Source of truth:** this document consolidates definitions from the four source files listed at the top. When definitions conflict, update the source file first, then sync this registry.
