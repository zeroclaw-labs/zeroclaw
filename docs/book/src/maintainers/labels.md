# Labels

Single reference for every label used on PRs and issues. Sources of truth:

- `.github/labeler.yml`: path-label config consumed by `actions/labeler`
- `.github/label-policy.json`: contributor tier thresholds
- This page: definitions, behavior, and what's automated vs manual

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
- Project board fields are appropriate for issue planning stage, active owner or steward path, dependency state, stale-exemption reason, and roadmap grouping when those fields are actively maintained.
- Native GitHub PR state owns fast-changing review state: review decision, required checks, mergeability, conflicts, and stale approvals.

The board should reduce maintainer work. If a field would need manual upkeep after every PR push or review, prefer labels, milestones, or native GitHub state instead.

Labels can suggest likely ownership, but they are not ownership. A `channel:*`, `provider:*`, `tool:*`, `security`, or `docs` label identifies the surface that probably needs attention. Contributor-visible owner-source rules live in the [Project board contract](./pr-workflow.md#project-board-contract).

Use assignees for active work. Use area stewardship for routing responsibility when nobody is implementing yet. The [Project board contract](./pr-workflow.md#issue-ownership-path) defines the accepted owner sources and routing outcomes.

## Canonical spelling

Use the live no-space module spelling for scoped module labels: `provider:openai`, `channel:telegram`, `security:policy`, and similar labels. The size, risk, and most type families currently keep a space after the colon: `size: XS`, `risk: low`, `risk: medium`, `risk: high`, and `type: docs`.

Legacy duplicate labels such as `provider: openai`, `channel: telegram`, or `tool: shell` are cleanup candidates. Future no-space spelling for size, risk, or type labels is also a migration question, not a casual rename. Migrate open issues/PRs to the canonical label before deletion. Do not delete labels with open references, broadly rename label families, or remove stale-policy labels without a maintainer decision for that cleanup batch.

## Automation contract

Live PR label automation is split by source. `pr-path-labeler.yml` runs `actions/labeler` from `.github/labeler.yml` on PR open, reopen, and every pushed update. Because that workflow uses `sync-labels: true`, labels owned by `.github/labeler.yml` are recalculated from the current PR file set: matching path labels are added, and path labels that no longer match are removed.

Dependabot also seeds configured labels on its own PRs from `.github/dependabot.yml`: Cargo updates get `dependencies`; GitHub Actions and Docker updates get `ci` and `dependencies`. Those labels are initial Dependabot PR metadata, not the synchronized path-labeler contract.

Today `.github/labeler.yml` owns only path and scope labels such as `docs`, `ci`, `channel`, `provider:openai`, and `tool:file`. It does not own `risk:*`, `size:*`, `type:*`, contributor-tier, status, resolution, stale, or pickup labels.

If risk or size automation is added later, it should recalculate on every pushed PR update so the labels continue to describe the actual diff under review. Risk automation must honor `risk: manual` as an override that prevents future automated risk replacement for that PR until a maintainer removes the override.

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
| `type:rfc` | RFC issue or proposal; protected from stale closure while active |

## Path labels

Applied automatically by `pr-path-labeler.yml`. Globs live in `.github/labeler.yml`; when this page and the config disagree, treat `.github/labeler.yml` as the operational source and update this page.

### Base scope labels

| Label | Matches |
|---|---|
| `docs` | `docs/**`, `**/*.md`, `**/*.mdx`, `LICENSE`, `.markdownlint-cli2.yaml` |
| `dependencies` | `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.github/dependabot.yml` |
| `ci` | `.github/codeql/**`, `.github/workflows/**`, `.github/*.yaml`, `.github/*.yml`, `.github/*.json`, `.githooks/**` |
| `core` | `src/*.rs` |
| `agent` | `src/agent/**`, `crates/zeroclaw-runtime/src/agent/**` |
| `channel` | `src/channels/**`, `crates/zeroclaw-channels/src/**` |
| `gateway` | `src/gateway/**`, `crates/zeroclaw-gateway/src/**` |
| `config` | `src/config/**`, `crates/zeroclaw-config/src/**` |
| `cron` | `src/cron/**`, `crates/zeroclaw-runtime/src/cron/**` |
| `daemon` | `src/daemon/**`, `crates/zeroclaw-runtime/src/daemon/**` |
| `doctor` | `src/doctor/**`, `crates/zeroclaw-runtime/src/doctor/**` |
| `health` | `src/health/**`, `crates/zeroclaw-runtime/src/health/**` |
| `heartbeat` | `src/heartbeat/**`, `crates/zeroclaw-runtime/src/heartbeat/**` |
| `integration` | `src/integrations/**`, `crates/zeroclaw-runtime/src/integrations/**` |
| `memory` | `src/memory/**`, `crates/zeroclaw-memory/src/**` |
| `security` | `src/security/**`, `crates/zeroclaw-runtime/src/security/**` |
| `runtime` | `src/runtime/**`, `crates/zeroclaw-runtime/src/**` |
| `quickstart` | `crates/zeroclaw-runtime/src/quickstart/**`, `crates/zeroclaw-gateway/src/api_quickstart.rs`, `apps/zerocode/src/quickstart_pane.rs`, `web/src/pages/quickstart/**` |
| `provider` | `src/providers/**`, `crates/zeroclaw-providers/src/**` |
| `service` | `src/service/**`, `crates/zeroclaw-runtime/src/service/**` |
| `skillforge` | `src/skillforge/**`, `crates/zeroclaw-runtime/src/skillforge/**` |
| `skills` | `src/skills/**`, `crates/zeroclaw-runtime/src/skills/**` |
| `tool` | `src/tools/**`, `crates/zeroclaw-tools/src/**` |
| `tunnel` | `src/tunnel/**`, `crates/zeroclaw-runtime/src/tunnel/**` |
| `observability` | `src/observability/**`, `crates/zeroclaw-runtime/src/observability/**` |
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
| `channel:wecom` | `wecom.rs`, `wecom_ws.rs` |
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

Based on effective changed line count, normalized for docs-only and lockfile-heavy PRs. Currently applied **manually**; the size automation that previously computed these was removed during CI simplification. Future size automation should follow the [automation contract](#automation-contract).

| Label | Threshold |
|---|---|
| `size: XS` | ≤ 80 lines |
| `size: S` | ≤ 250 lines |
| `size: M` | ≤ 500 lines |
| `size: L` | ≤ 1000 lines |
| `size: XL` | > 1000 lines |

## Risk labels

For PRs, risk labels describe the actual diff under review: touched paths, behavior change, security boundary exposure, and rollback difficulty. For issues, risk labels describe the likely fix blast radius based on the report, help triage reviewer depth and contributor fit, and may change once a concrete PR shows the actual implementation path. Currently applied **manually**. Future risk automation should follow the [automation contract](#automation-contract).

| Label | Meaning |
|---|---|
| `risk: low` | No high-risk paths touched, small change |
| `risk: medium` | Behavioral `crates/*/src/**` changes without boundary or security impact |
| `risk: high` | Touches a high-risk path, or large security-adjacent change |
| `risk: manual` | Maintainer override that freezes automated risk recalculation |

High-risk paths (canonical set; other maintainer pages reference this list): `crates/zeroclaw-runtime/src/**`, `crates/zeroclaw-gateway/src/**`, `crates/zeroclaw-tools/src/**`, `crates/zeroclaw-runtime/src/security/**`, `.github/workflows/**`.

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
| `status:no-stale` | Explicit stale exemption for accepted or otherwise long-lived work that is not already protected by another stale exclusion. Target policy: use only when the [Project board contract](./pr-workflow.md#issue-ownership-path) has a contributor-visible stale-exemption reason and owner path. Active release trackers and active RFC or design trackers may use the tracker itself as the visible reason and steward surface while they remain active; revisit them when the milestone closes, the tracker drifts from live state, the RFC reaches a decision, is superseded, or closes, or the issue stops representing an active project decision surface. Existing exemptions missing those facts should be audited and repaired before stale sweeps stop honoring them. |

## Resolution labels

Resolution labels explain why an issue or PR is being closed or removed from the active queue. They are terminal outcomes, not lifecycle status labels, and should include enough comment context for a future maintainer to understand the decision.

| Label | Purpose |
|---|---|
| `wontfix` | Valid request or report that the project is explicitly choosing not to pursue. Use a brief rationale; do not silently close. |
| `invalid` | Not actionable as a bug, feature request, support item, RFC, or tracked project work. Explain the mismatch or missing requirement. |
| `duplicate` | Same underlying issue as another tracked issue or PR. Link the canonical target before closing or redirecting discussion. |

Do not create or apply proposed terminal labels such as `status:wont-do` or `status:wont-fix` until a maintainer-approved label migration packet defines the exact rename, alias, or deletion plan. The current live label for the board-level "Won't Do" concept is `wontfix`.

Superseding is a replacement process, not currently a live label. Use [Superseding PRs](./superseding.md) for replacement rules and attribution requirements until a later approved migration packet creates or maps a superseding label.

## Triage labels

Applied manually: the auto-response automation that used to handle these was removed during CI simplification.

| Label | Purpose |
|---|---|
| `r:needs-repro` | Incomplete bug report; request a deterministic repro |
| `r:support` | Usage / help item better handled outside the bug backlog |
| `stale-candidate` | Dormant PR or issue; candidate for closing |

## Community pickup labels

Applied manually when maintainers want outside contribution.

| Label | Purpose |
|---|---|
| `good first issue` | Small, self-contained, well-documented XS/S work that is safe for a new contributor and has acceptance criteria, relevant code or docs links, and a named mentor or contact |
| `help wanted` | Actionable, unblocked work that maintainers want external help on and can review, usually low or medium likely issue risk |

Do not use `help wanted` as a generic marker for "valid but unstaffed." If an issue is blocked, architecture-dependent, missing acceptance criteria, likely high-risk, or waiting on a policy decision, leave it without pickup labels until the blocker is resolved or a maintainer writes the missing scope.

## Maintenance triggers

Update this page when:

- A new channel, provider, or tool is added to the source tree (path labels need new entries).
- A label policy or threshold changes.
- A new triage workflow surfaces or an old one is removed.

The automation status notes ("currently applied manually") are deliberately included so a future maintainer doesn't assume the absence of a workflow means the label tier doesn't exist.
