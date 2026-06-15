# Communication

Where to ask questions, file bugs, propose features, and reach the team.

**If you just want to talk to us, Discord is the answer.** For anything that needs a durable record (bugs, feature requests, design discussion, RFCs), GitHub.

## Discord: best place to reach the team

Real-time chat. This is where the maintainers live day-to-day; the fastest path to a human response.

Channels:

- `#general`: the default room
- `#help`: "I can't get X working" threads; the fastest way to unblock
- `#dev`: in-flight development discussion
- `#releases`: announcements, release notes, breaking-change pre-warnings

[Invite link in the repo README.](https://github.com/zeroclaw-labs/zeroclaw)

**Discord is ephemeral**: if the conversation leads to a bug or a feature idea, capture it as a GitHub issue afterwards so the record persists. Discord is for conversation; GitHub is for memory.

Use a GitHub handoff when Discord produces something the project must remember. Create or update an issue, discussion, PR comment, or maintainer doc when the thread produces a reproducible bug, concrete feature scope, architecture or governance decision, maintainer commitment, owner assignment, milestone decision, blocker, workaround, validation evidence, release-impact note, or stale-exemption reason. The handoff only needs the decision, evidence, owner when one exists, and enough context for another maintainer to continue without rereading chat.

## GitHub issues

For bugs, feature requests, and anything that needs to be tracked.

- **Bug reports**: use the bug template (`.github/ISSUE_TEMPLATE/bug_report.yml`). Include `zeroclaw --version`, OS, and the output of `zeroclaw doctor`.
- **Feature requests**: use the feature template (`.github/ISSUE_TEMPLATE/feature_request.yml`). Focus on user value and constraints; implementation details are for RFCs or PR discussion.
- **RFCs**: see [RFC process](./rfcs.md).

Search before filing. Duplicates get consolidated; the search box is your friend.

## GitHub Discussions

For community-facing threads that need more permanence than Discord but are not yet tracked work. Discussions work well for Q&A, ideas, project show-and-tell, polls, maintainer announcements, and "does anyone else see this?" threads where Discord would scroll away.

Treat Discussions as non-urgent community conversation. They are maintained intake only when a steward or review cadence is documented. The maintainer routine and default cadence live in [Reviewer playbook: Discussions stewardship](../maintainers/reviewer-playbook.md#discussions-stewardship).

Discussions are part of the GitHub handoff system, not a replacement for issues, RFCs, PR comments, or maintainer docs. Move a Discussion into the tracked surface once it produces a concrete bug, feature scope, owner, blocker, validation evidence, policy decision, or docs requirement.

Use this split when choosing a surface:

| Surface | Use it for | Move it when |
|---|---|---|
| Discord | Fast help, live coordination, early "is this a thing?" conversation | The project needs a durable record, decision, owner, validation note, blocker, or release-impact note |
| Discussions | Searchable Q&A, ideas, show-and-tell, demos, polls, announcements, broad feedback, and exploratory architecture questions that are not ready for formal tracking | The thread produces a concrete bug, feature scope, architecture proposal, policy decision, docs gap, owner, blocker, or validation evidence |
| Issues | Bugs, feature requests, support/configuration reports, contributor tasks, roadmap trackers, and other work that needs triage or tracking | The issue turns into an RFC, PR, tracker item, duplicate, support redirect, or closure decision |
| RFC issues | Architecture, governance, lifecycle, compatibility, or process decisions that need formal review | The RFC is accepted, rejected, superseded, or split into implementation issues |
| PR comments | Review feedback and implementation details for an active change | The detail becomes durable policy, reusable docs, a follow-up issue, or release note |

Discussion categories should make the expected outcome obvious. Use Q&A for answerable questions, Ideas for proposals that need community shaping, Show and tell for project-related demos, integrations, or downstream forks people want to share, Polls for community votes, Announcements for maintainer updates, and General for broad searchable conversation, early architecture exploration, or downstream/fork/enterprise collaboration that is not yet tracked work. If downstream collaboration becomes recurring enough to need its own stewarded lane, maintainers can add a dedicated category later.

Close the loop when a Discussion moves. Add a short summary and link to the issue, RFC, PR, or doc that now owns the outcome. If the category supports accepted answers, mark the summary or tracked-work link as the answer when that accurately reflects the result.

[github.com/zeroclaw-labs/zeroclaw/discussions](https://github.com/zeroclaw-labs/zeroclaw/discussions)

## Maintainer contacts

Core maintainers and their focus areas:

| Handle | Role | Focus |
|---|---|---|
| [@JordanTheJet](https://github.com/JordanTheJet) | Project lead | Hardware, edge deployments |
| [@theonlyhennygod](https://github.com/theonlyhennygod) | Original creator | Channels, gateway |
| [@WareWolf-MoonWall](https://github.com/WareWolf-MoonWall) | Maintainer | Governance, docs, reviewer playbook |
| [@singlerider](https://github.com/singlerider) | Maintainer | Runtime, providers, infra |

`@`-mention sparingly, CC maintainers only when the issue genuinely needs their attention. Default to letting the team triage.

## Security issues

Do not file public issues for security vulnerabilities.

Report privately through GitHub's [private vulnerability reporting](https://github.com/zeroclaw-labs/zeroclaw/security/advisories/new) (Security Advisories).

Include:

- Affected versions
- Reproduction (minimal, please)
- Impact assessment

We aim to acknowledge within 48 hours, assess within 1 week, and ship a fix within 2 weeks for critical issues. Coordinated disclosure is appreciated.

See `SECURITY.md` in the repo root for the full policy.

## Release feed

Subscribe to the GitHub release feed to be notified when new versions ship:

```
https://github.com/zeroclaw-labs/zeroclaw/releases.atom
```

Or watch the repo on GitHub (Watch → Custom → Releases).

Release notes are cross-posted to Discord `#releases` and the community Twitter.

## Commercial support

None offered. ZeroClaw is maintained by the community. If you're deploying at scale and want SLAs, sponsor a maintainer directly or fund a dedicated support arrangement through the core team. Reach out via `hello@zeroclaw.dev`.

## Feedback

Open-ended feedback, "I tried to do X and it felt wrong", UX observations, direction thoughts, lands best as a thread in Discord `#general` or `#dev` when it needs a fast live conversation. Use GitHub Discussions `General` or `Ideas` when the feedback should stay searchable for asynchronous community input. If the thread turns into something concrete, move it to an issue, RFC, PR comment, or doc.

## Contributor recognition

Everyone who's had a PR merged appears in the contributors list on the repo. For substantial contributions, features, RFCs, significant bug fixes, your handle shows up in the release notes.

## See also

- [How to contribute](./how-to.md)
- [RFC process](./rfcs.md)
- [Philosophy](../philosophy/index.md): what the project is trying to be, so you know what's in scope
