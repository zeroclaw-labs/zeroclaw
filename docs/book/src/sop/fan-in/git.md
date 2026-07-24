# SOP Fan-In: Git

Git forge events can start SOP runs. When an event type is routed to `sop` in the channel's event table, the Git channel lifts the normalized forge event into a SOP event (the topic is `git.<alias>:<event_type>`, the structured JSON payload carries the repo/issue/PR fields) and dispatches it to the engine. Only the **mention gate** is relaxed for `sop`-routed lifecycle events: a `pull_request.opened` routed to a triage SOP fires whether or not the author mentioned the app. The **author checks still apply** to every delivery, whether SOP or conversational: events from the app's own account or from other bots are dropped (unless `listen_to_bots` is set), and the peer-group allowlist is enforced on the actor's login. In other words, `sop` routes bypass `mention_only` but not self/bot filtering or the author allowlist.

> The transport side (provider, forge auth, polling, repository scoping, event routes) is configured on the [Git channel](../../channels/git.md). This page covers the trigger. Which event types reach SOP ingress is decided by the channel's per-event `events` route table.

## Trigger

{{#sop-trigger channel}}

## Matching

The `topic` is matched exactly against the event topic the channel emits, `git.<alias>:<event_type>` (for example `git.main:pull_request.opened`). The event's structured JSON payload is forwarded into the SOP event and is available to an optional trigger `condition`; step context receives the capped, sanitized, framed form. A JSON-path `condition` such as `$.repo == "octo/repo"` narrows a SOP to one repository. Known event types are `issue_comment.created`, `issues.opened`, `pull_request.opened`, `pull_request.closed`, `pull_request.merged`, `pull_request_review_comment.created`, `workflow_run.completed`, `workflow_run.failed`, and `release.published`.

## Fire it

Route an event type to a SOP on the channel (an `events` entry with `sop = "<name>"`), load a SOP whose `channel` trigger names the matching topic, then cause the forge event: open or comment on an issue/PR, publish a release, or let a workflow run finish. The channel normalizes the event, screens the payload for safety, and dispatches to every loaded SOP whose `topic` matches and whose `condition` (if any) holds. Routing an event type is also what subscribes the channel to that forge endpoint, so only routed event types are polled.

If nothing starts, confirm the event type is routed to `sop` (not left at the conversational default), the SOP's `channel` trigger topic matches `git.<alias>:<event_type>` exactly, and the `condition` holds against the payload. See the [fan-in overview troubleshooting table](./overview.md#troubleshooting).

## Approve and observe

Runs that hit a checkpoint pause as `WaitingApproval`. Clear or inspect them with the CLI (`zeroclaw sop list`, `zeroclaw sop approve`) or out-of-band over the [gateway API](../../gateway/api.md) approval endpoints (`GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`).

## See also

- [Git channel](../../channels/git.md): provider, forge auth, polling, event routes
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
