# Labels

Single reference for every label used on PRs and issues. Sources of truth:

- `.github/labeler.yml` — path-label config consumed by `actions/labeler`
- `.github/label-policy.json` — contributor tier thresholds
- This page — definitions, behavior, and what's automated vs manual

When definitions conflict, update the source file first, then sync this page.

## Ownership boundaries

Labels are portable metadata. They should answer what kind of work this is, what code area it touches, how risky it is to review, and whether stale policy or triage policy needs special handling.

When Project board automation is added, use it as an automated planning board,
not as a second PR review queue. The board should answer slower-moving planning
questions: what is ready to pick up, who owns it, what tracker or milestone it
belongs to, and what is blocked. Native GitHub PR state should continue to
answer fast-moving review and merge questions.

Keep the split based on update frequency:

- Labels own durable classification: work type, scope/component, review risk, measured PR size, and stale exemption.
- Project board fields are appropriate for issue planning stage, active owner, dependency state, and roadmap grouping when those fields are actively maintained.
- Native GitHub PR state owns fast-changing review state: review decision, required checks, mergeability, conflicts, and stale approvals.

The board should reduce maintainer work. If a field would need manual upkeep after every PR push or review, prefer labels, milestones, or native GitHub state instead.

## Canonical spelling

Use the live no-space module spelling for scoped module labels: `provider:openai`, `channel:telegram`, `tool:shell`, `security:policy`, and similar labels. The size and risk families intentionally keep a space after the colon: `size: XS`, `risk: low`, `risk: medium`, `risk: high`.

Legacy duplicate labels such as `provider: openai`, `channel: telegram`, or `tool: shell` are cleanup candidates. Migrate open issues/PRs to the canonical no-space spelling before deletion. Do not delete labels with open references, broadly rename label families, or remove stale-policy labels without a maintainer decision for that cleanup batch.

## Cleanup protocol

Label cleanup is a maintainer action, not a side effect of normal PR review.

Use this sequence:

1. Refresh live label usage before acting.
2. Split candidates into zero-history deletes, zero-open duplicate deletes, migrate-first active labels, and policy holdbacks.
3. For labels with open refs, add the canonical label to each open issue/PR, remove the legacy label, verify the legacy label has zero open refs, then delete it.
4. Do not delete governance labels, stale-policy labels, contributor-tier labels, or default GitHub labels as part of module-label cleanup.

Every live cleanup batch needs exact maintainer approval for the labels and issue/PR refs being changed.

## Type labels

Type labels capture the high-level work class. They are separate from path labels such as `docs`, `ci`, or `dependencies`.

| Label | Purpose |
|---|---|
| `type: ci` | CI, workflow, or repository automation work |
| `type: dependencies` | Dependency or lockfile maintenance |
| `type: docs` | Documentation-only or docs-primary work |
| `type:rfc` | RFC issue or proposal; protected from stale closure |

## Path labels

Applied automatically by `pr-path-labeler.yml` (the only labeling automation currently active). Globs live in `.github/labeler.yml`.

### Base scope labels

| Label | Matches |
|---|---|
| `docs` | `docs/**`, `**/*.md`, `**/*.mdx`, `LICENSE`, `.markdownlint-cli2.yaml` |
| `dependencies` | `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.github/dependabot.yml` |
| `ci` | `.github/codeql/**`, `.github/workflows/**`, `.github/*.yaml`, `.github/*.yml`, `.github/*.json`, `.githooks/**` |
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

`ci` is scoped to GitHub automation/config files, not all `.github/**` paths. The root `.github/*.json` matcher is intentional for automation metadata (for example `.github/label-policy.json`), so files like `.github/assets/**`, `.github/ISSUE_TEMPLATE/**`, `.github/CODEOWNERS`, and `.github/pull_request_template.md` do not match `ci`.

### Per-channel labels

Each channel gets a `channel:<name>` label in addition to the base `channel` label.

| Label | Matches |
|---|---|
| `channel:bluesky` | `bluesky.rs` |
| `channel:clawdtalk` | `clawdtalk.rs` |
| `channel:cli` | `cli.rs` |
| `channel:dingtalk` | `dingtalk.rs` |
| `channel:discord` | `discord.rs` |
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

Track lifecycle state of RFCs and tracked work items. Applied manually unless a maintained workflow says otherwise.

| Label | Description |
|---|---|
| `status:accepted` | RFC or work item ratified by the team. This does not exempt the issue from stale handling by itself. |
| `status:blocked` | Work is valid but waiting on an external dependency, maintainer decision, or linked prerequisite. Exempt from stale while the blocker is recorded and unresolved. Do not pair with `status:no-stale` for the same blocker. |
| `status:in-progress` | An open PR is actively targeting this issue. Reconcile against live PR state during stale passes; the label is not a permanent exemption after the PR closes. |
| `status:stale` | No author activity for the stale window; may close if not refreshed |
| `status:no-stale` | Explicit stale exemption for accepted or otherwise long-lived work that is not already protected by another stale exclusion. Use only when a maintainer comment, issue body, or tracker entry records why the issue should stay open. |

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

## Maintenance triggers

Update this page when:

- A new channel, provider, or tool is added to the source tree (path labels need new entries).
- A label policy or threshold changes.
- A new triage workflow surfaces or an old one is removed.

The automation status notes ("currently applied manually") are deliberately included so a future maintainer doesn't assume the absence of a workflow means the label tier doesn't exist.
