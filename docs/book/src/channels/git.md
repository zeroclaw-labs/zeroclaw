# Git

Converse with the agent through a git forge's issue and pull-request comments, and surface repository events, including PR lifecycle, review comments, CI outcomes, and releases, through a per-event routing table. The channel is built around a **provider seam**: a `provider` field selects the forge. GitHub, Gitea, and Forgejo are wired providers; additional forges drop in as sibling providers without changing the generic channel.

> **New to ZeroClaw?** Start with the [Quickstart](../getting-started/quickstart.md) to get a running agent, then skim [Concepts](../getting-started/concepts.md) for the terms (agent, peer group, autonomy, SOP) this page assumes.

With the GitHub provider, ZeroClaw authenticates as a **GitHub App** and replies as the app's own bot identity (`your-app[bot]`), so it works on any repository the app is installed on. There is no personal access token and no shared user account.

With the Gitea/Forgejo provider, ZeroClaw authenticates with a personal access token against the instance's Gitea-compatible API and replies as the token owner.

> **Build note:** the git channel is **not included** in the lean default build. Build with `--features channel-git` (or `channels-full`). The `channel-git` feature pulls in every wired forge provider in one build, so a single binary serves all supported forges; there is no smaller per-provider build subset to select.

## Who can talk to the agent

{{#peer-group git}}

## How it works

- **Polling, not webhooks.** The channel polls the forge REST API for new issues, pull requests, and comments on a `since` cursor. The daemon needs no public URL, tunnel, or inbound exposure; it works behind NAT.
- **Issue-scoped conversations.** Every message on the same issue or PR shares one conversation thread; the agent replies as a comment on that issue.
- **Streaming replies.** The agent posts a draft comment and edits it in place as the response grows (edits are spaced ≥ 2 s to respect forge abuse limits).
- **Reactions.** Acknowledgement reactions map onto the forge's reaction set (with GitHub: 👀 → `eyes`, ✅ → `+1`, ⚠️ → `confused`, …); unmappable emoji are skipped.
- **Cold start.** Events created before the daemon started are never processed, so restarting can't replay history. The flip side: comments posted while the daemon was down are missed, so mention the app again.
- **Comment edits are ignored.** Only newly created comments and issue/PR opening posts trigger the agent.

## Credentials

Each provider authenticates differently, and each has a full step-by-step walkthrough:

- **GitHub** authenticates as a GitHub App (App ID plus a generated private key). See [Creating a GitHub App](./git-github-app.md).
- **Gitea / Forgejo** authenticate with a personal access token against the instance's Gitea-compatible API. See [Creating a Gitea / Forgejo token (Codeberg)](./git-gitea-forgejo.md).

## Configure

Set the channel fields on whichever surface you prefer:

{{#config-where channels git}}

The full field reference, straight from the schema:

{{#config-fields channels.git}}

The `default` alias is the common first instance. Leave `repos` empty to poll every repository visible to the credential, or set it to an explicit repository list for lower rate usage. Set `listen_to_bots` only if comments from other bot accounts should be processed. An unknown `provider` value is a clear startup error rather than a silent fallback.

Credential setup differs per provider and each has its own encrypted secret; both walkthroughs cover it end to end:

- **GitHub:** App ID plus a private key. See [Creating a GitHub App](./git-github-app.md).
- **Gitea / Forgejo:** an access token and API base URL. See [Creating a Gitea / Forgejo token (Codeberg)](./git-gitea-forgejo.md).

## Events & routing

Beyond conversation, the channel normalizes repository activity into typed events and routes each event type per config. Routing an event to a `sop` dispatches it to a [Standard Operating Procedure](../sop/index.md), a deterministic, auditable procedure with trigger matching and approval gates. The [Git SOP fan-in](../sop/fan-in/git.md) page details exactly how a forge event becomes a SOP run.

| Event type | Example route | Result |
| --- | --- | --- |
| `pull_request.opened` | `sop` = `pr-triage` | Dispatches the PR payload to the `pr-triage` SOP. |
| `issues.opened` | `sop` = `issue-triage` | Dispatches the issue payload to the `issue-triage` SOP. |
| `issue_comment.created` | `message` = `true` | Delivers the comment to the normal conversational agent loop. |
| `workflow_run.failed` | `sop` = `ci-failure` | Dispatches the CI failure payload to SOP ingress. |
| `release.published` | `message` = `true` | Delivers the release event to the normal agent loop. |

Known event types: `issue_comment.created`, `issues.opened`, `pull_request.opened`, `pull_request.closed`, `pull_request.merged`, `pull_request_review_comment.created`, `workflow_run.completed`, `workflow_run.failed`, `release.published`.

- **Defaults.** With no `events` table, the channel behaves conversationally: `issue_comment.created`, `issues.opened`, and `pull_request.opened` are delivered as messages (mention-gated as described above); everything else is ignored. Event types absent from a non-empty table get the same per-type defaults: listing `workflow_run.failed` doesn't turn conversation off. An entry with neither `message = true` nor a `sop` explicitly disables that event type.
- **Routing an event type is subscribing to it.** The channel derives which API endpoints to poll from the table: review comments, releases, and Actions runs are only fetched when their event types are routed, so an unconfigured channel costs exactly what it did before. GitHub currently covers every listed event type; the Gitea/Forgejo provider covers issue comments, issue/PR openings, PR close/merge transitions, releases, replies, edits, deletes, and reactions.
- **`sop` routing.** A `sop` route emits a channel-sourced SOP event with topic `git.<alias>:<event_type>` and a structured JSON payload. The routed event is consumed by SOP ingress rather than delivered as chat. Match it from `SOP.toml` with a `channel` trigger whose topic is the Git event topic, for example `git.main:pull_request.opened`; use an optional condition such as `$.repo == "octo/repo"` to narrow the SOP to one repository.
- **Mention gating per route.** The `mention_only` gate applies to conversational events on the message path. `sop`-routed events skip it: a PR routed to `pr-triage` is captured whether or not the author mentioned the app. Lifecycle/CI/release events have no mention surface and are never gated. The app's own activity is always dropped; other bots' activity follows `listen_to_bots`; every delivery passes the peer-group allowlist on the author's login.
- **Reply surfaces.** Comment, issue, and PR events reply onto their issue/PR thread. Workflow-run events reply onto the run's associated PR when the forge reports one; otherwise, and for releases, the target is the bare repository and the agent cannot reply on-platform (route those to a SOP or act through other tools).
- **Events API backbone (optional, GitHub).** `events_backbone = true` additionally polls `/repos/{owner}/{repo}/events` with ETag-conditional requests (one request per repo per tick; an idle repo answers 304 and costs almost nothing). Caveats: the feed lags by up to ~5 minutes, payloads are trimmed, and **Actions events never appear in it**: workflow runs always use their dedicated endpoint. Anything surfaced by both the feed and a targeted endpoint is de-duplicated, so it's safe to combine. Gitea/Forgejo ignore this option today.

## Operating notes

- **Rate budget:** on GitHub, each installation gets 5,000 requests/hour; the conversational default spends 2 requests per repository per poll tick (5 repos at a 30 s interval ≈ 1,200/hour). Each additionally routed endpoint family (review comments, releases, Actions runs) adds 1 per repo per tick, and the Events API backbone adds 1 conditional request (304s on idle repos are effectively free). Gitea/Forgejo rate limits depend on the instance. On a rate-limit response the channel backs off until the limit window resets.
- **Many repositories:** when `repos` is empty and the credential can see more than 100 repositories, only the first page is polled (a warning is logged for GitHub). List `repos` explicitly in that case.

## Safety

Issues and PR comments on public repositories are adversarial input. Keep `mention_only = true`, gate senders with a [peer group](./peer-groups.md) (an empty peer set denies everyone, `["*"]` accepts anyone), and keep [autonomy](../security/autonomy.md) at `Supervised` or lower for public-facing repositories. This is the same guidance as [social channels](./social.md#operating-social-channels-safely).

## Related docs

- **Set up credentials:** [Creating a GitHub App](./git-github-app.md) · [Creating a Gitea / Forgejo token (Codeberg)](./git-gitea-forgejo.md)
- **Who can reach the agent:** [Peer Groups](./peer-groups.md)
- **What the agent may do:** [Security & Autonomy](../security/overview.md) · [Autonomy levels](../security/autonomy.md)
- **Event-driven automation:** [Standard Operating Procedures](../sop/index.md) · [Git SOP fan-in](../sop/fan-in/git.md)
- **The bigger picture:** [Agents](../agents/overview.md) · [Channels overview](./overview.md)
