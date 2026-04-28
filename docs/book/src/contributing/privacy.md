# Privacy and PII Discipline

ZeroClaw artifacts are public — git history, releases, fixtures, snapshots, the docs book, every rendered locale. Anything you commit ships with the project forever. Treat privacy as a merge gate, not best-effort.

## Never commit any of these

In code, docs, tests, fixtures, snapshots, logs, examples, error messages, or commit messages:

- Real names
- Personal email addresses
- Phone numbers, addresses
- Access tokens, API keys, credentials
- Account IDs, session IDs, anything that identifies a real person or account
- Private URLs (internal hostnames, signed S3 URLs, anything not meant to be public)

This list isn't exhaustive. The principle: if it would identify a real person or grant access to something, it doesn't belong in the repo.

## Use neutral placeholders

Test fixtures, examples, error messages, and snapshots use generic project-scoped placeholders instead of real identity data. Recommended palette:

| Use case | Examples |
|---|---|
| Actor labels | `zeroclaw_user`, `zeroclaw_operator`, `zeroclaw_maintainer`, `test_user`, `user_a`, `project_bot` |
| Service / runtime labels | `zeroclaw_bot`, `zeroclaw_service`, `zeroclaw_runtime`, `zeroclaw_node` |
| Environment labels | `zeroclaw_project`, `zeroclaw_workspace`, `zeroclaw_channel` |
| Hostnames | `example.com`, `host.invalid`, `192.0.2.x` (RFC 5737 documentation range) |
| Email addresses | `user@example.com`, `bot@zeroclaw.invalid` |

Test names, assertion messages, and fixture content stay impersonal and system-focused — avoid first-person language and identity-specific framing.

## When you have to reference identity

If a test or doc genuinely needs a role-shaped identity, use ZeroClaw-scoped roles only: `ZeroClawAgent`, `ZeroClawOperator`, `ZeroClawMaintainer`. Don't borrow real names, even pseudonyms — pseudonyms drift back into being real over time.

GitHub `@`-mentions in PR/issue comments are different — addressing a contributor by their handle is how you talk to people on GitHub, and `@WareWolf-MoonWall` is not a privacy violation. The rule is about **content stored in the repo** (code, tests, fixtures, docs), not about conversation in PR/issue threads.

## Reproducing external incidents

If you're capturing an incident trace, log payload, or external response in a test fixture: redact and anonymize before committing. Real session IDs, real user IDs, real hostnames, and real auth tokens all need to go through a scrubbing pass first. The redacted version is what ships; the original stays out of git.

## Pre-push checklist

Before pushing, scan the staged diff specifically for identity leakage:

```bash
git diff --cached
```

The shapes to look for: anything that looks like an email, a URL with a non-public hostname, a long random-looking string that might be a token, a name that isn't yours and didn't come from a project-scoped placeholder.

If a CI run captured a real value (a real session ID in a snapshot, a real user agent string with identifying info, etc.) and got committed, it's a privacy incident — open an issue, scrub, force-push if it just landed, and contact the maintainers if it landed on `master`.

## Why this is strict

The last category — accidentally committing a real identity — is hard to undo. Once a real name or email lands on `master` it propagates through forks, mirrors, and clones immediately. Squashing or force-pushing fixes the public branch but doesn't reach the copies. The cheapest fix is the pre-commit scan; everything after that is harm reduction.
