# Factory Inspector Policy

## Decision Classes

| Class | Action |
|---|---|
| `AUTO_COMMENT` | May comment in `comment-only`. |
| `QUEUE_FOR_REVIEW` | Human or agent review required; no mutation. |
| `NO_ACTION` | Ignore. |

## Comment Authority

Factory Inspector may comment only on open PRs and only for deterministic intake issues:

- missing required PR template sections;
- missing or placeholder linked issue section;
- missing validation evidence;
- missing risk label;
- high-risk touched paths without `risk: high` or `risk: manual`;
- missing rollback details for `risk: medium` or `risk: high`;
- placeholder security/privacy section.

It must not:

- approve, request changes, close, merge, edit branches, or edit PR bodies;
- comment on semantic code correctness;
- comment on issue intake automatically in the first version;
- duplicate an earlier Factory Inspector comment with the same hidden marker.

## High-Risk Paths

Treat these as high-risk until manually overridden:

- `crates/zeroclaw-runtime/src/security/`
- `crates/zeroclaw-runtime/`
- `crates/zeroclaw-gateway/`
- `crates/zeroclaw-tools/`
- `.github/workflows/`

## Output Shape

Comments should be one checklist with concrete next steps. Keep under 200 words unless the PR has unusually broad intake failures.
