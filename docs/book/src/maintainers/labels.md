# Labels

Single reference for every label used on PRs and issues. Sources of truth:

- `.github/labeler.yml` — path-label config consumed by `actions/labeler`
- `.github/label-policy.json` — contributor tier thresholds
- This page — definitions, behavior, and what's automated vs manual

When definitions conflict, update the source file first, then sync this page.

## Path labels

Applied automatically by `pr-path-labeler.yml` (the only labeling automation currently active). Globs live in `.github/labeler.yml`.

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

### Per-channel labels

Each channel gets a `channel:<name>` label in addition to the base `channel` label.

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

### Per-provider labels

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
| `provider:atomic-chat` | `atomic_chat.rs` |

### Per-tool-group labels

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

## Size labels

Based on effective changed line count, normalized for docs-only and lockfile-heavy PRs. Currently applied **manually** — the size automation that previously computed these was removed during CI simplification.

| Label | Threshold |
|---|---|
| `size: XS` | ≤ 80 lines |
| `size: S` | ≤ 250 lines |
| `size: M` | ≤ 500 lines |
| `size: L` | ≤ 1000 lines |
| `size: XL` | > 1000 lines |

## Risk labels

Heuristic combining touched paths and change size. Currently applied **manually**.

| Label | Meaning |
|---|---|
| `risk: low` | No high-risk paths touched, small change |
| `risk: medium` | Behavioral `crates/*/src/**` changes without boundary or security impact |
| `risk: high` | Touches a high-risk path, or large security-adjacent change |
| `risk: manual` | Maintainer override that freezes automated risk recalculation |

High-risk paths: `crates/zeroclaw-runtime/src/**`, `crates/zeroclaw-gateway/src/**`, `crates/zeroclaw-tools/src/**`, `crates/zeroclaw-runtime/src/security/**`, `.github/workflows/**`.

When uncertain, treat as higher risk.

## Contributor tier labels

Defined in `.github/label-policy.json`. Based on the author's merged PR count queried from the GitHub API. Currently applied **manually**.

| Label | Minimum merged PRs |
|---|---|
| `trusted contributor` | 5 |
| `experienced contributor` | 10 |
| `principal contributor` | 20 |
| `distinguished contributor` | 50 |

## Status labels

Track lifecycle state of RFCs and tracked work items. Applied manually.

| Label | Description |
|---|---|
| `status:in-progress` | An open PR is actively targeting this issue |
| `status:accepted` | RFC or work item ratified by the team |

## Triage labels

Applied manually — the auto-response automation that used to handle these was removed during CI simplification.

| Label | Purpose |
|---|---|
| `r:needs-repro` | Incomplete bug report; request a deterministic repro |
| `r:support` | Usage / help item better handled outside the bug backlog |
| `invalid` | Not a valid bug or feature request |
| `duplicate` | Duplicate of an existing issue |
| `stale-candidate` | Dormant PR or issue; candidate for closing |
| `superseded` | Replaced by a newer PR |
| `no-stale` | Exempt from stale automation; accepted but blocked work |

## Maintenance triggers

Update this page when:

- A new channel, provider, or tool is added to the source tree (path labels need new entries).
- A label policy or threshold changes.
- A new triage workflow surfaces or an old one is removed.

The automation status notes ("currently applied manually") are deliberately included so a future maintainer doesn't assume the absence of a workflow means the label tier doesn't exist.
