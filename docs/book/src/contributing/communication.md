# Communication

Where to ask questions, file bugs, propose features, and reach the team.

**If you just want to talk to us, Discord is the answer.** For anything that needs a durable record (bugs, feature requests, design discussion, RFCs), GitHub.

## Discord — best place to reach the team

Real-time chat. This is where the maintainers live day-to-day; the fastest path to a human response.

Channels:

- `#general` — the default room
- `#help` — "I can't get X working" threads; the fastest way to unblock
- `#dev` — in-flight development discussion
- `#releases` — announcements, release notes, breaking-change pre-warnings

[Invite link in the repo README.](https://github.com/zeroclaw-labs/zeroclaw)

**Discord is ephemeral** — if the conversation leads to a bug or a feature idea, capture it as a GitHub issue afterwards so the record persists. Discord is for conversation; GitHub is for memory.

## GitHub issues

For bugs, feature requests, and anything that needs to be tracked.

- **Bug reports** — use the bug template (`.github/ISSUE_TEMPLATE/bug_report.yml`). Include `zeroclaw --version`, OS, and the output of `zeroclaw doctor`.
- **Feature requests** — use the feature template (`.github/ISSUE_TEMPLATE/feature_request.yml`). Focus on user value and constraints; implementation details are for RFCs or PR discussion.
- **RFCs** — see [RFC process](./rfcs.md).

Search before filing. Duplicates get consolidated; the search box is your friend.

## GitHub Discussions

For design chatter and conceptual conversations that aren't ready to be an issue. Works well for "does anyone else see this" posts and longer-form threads where Discord would scroll away.

[github.com/zeroclaw-labs/zeroclaw/discussions](https://github.com/zeroclaw-labs/zeroclaw/discussions)

## Maintainer contacts

Core maintainers and their focus areas:

| Handle | Role | Focus |
|---|---|---|
| [@JordanTheJet](https://github.com/JordanTheJet) | Project lead | Hardware, edge deployments |
| [@theonlyhennygod](https://github.com/theonlyhennygod) | Original creator | Channels, gateway |
| [@WareWolf-MoonWall](https://github.com/WareWolf-MoonWall) | Maintainer | Governance, docs, reviewer playbook |
| [@singlerider](https://github.com/singlerider) | Maintainer | Runtime, providers, infra |

`@`-mention sparingly — CC maintainers only when the issue genuinely needs their attention. Default to letting the team triage.

## Security issues

Do not file public issues for security vulnerabilities.

Email: `security@zeroclaw.dev`

Include:

- Affected versions
- Reproduction (minimal, please)
- Impact assessment

We aim to acknowledge within 48 hours and publish a patch + advisory within 14 days for critical issues. Coordinated disclosure is appreciated.

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

Open-ended feedback — "I tried to do X and it felt wrong", UX observations, direction thoughts — lands best as a thread in Discord `#general` or `#dev`. The team is more likely to see and discuss it there. If the thread turns into something concrete, move it to a GitHub Discussion or issue.

## Contributor recognition

Everyone who's had a PR merged appears in the contributors list on the repo. For substantial contributions — features, RFCs, significant bug fixes — your handle shows up in the release notes.

## See also

- [How to contribute](./how-to.md)
- [RFC process](./rfcs.md)
- [Philosophy](../philosophy.md) — what the project is trying to be, so you know what's in scope
