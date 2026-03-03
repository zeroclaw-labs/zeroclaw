# Pre-release Stage Gates

Workflow: `.github/workflows/pub-prerelease.yml`
Policy: `.github/release/prerelease-stage-gates.json`

## Stage Model

- `alpha`
- `beta`
- `rc`
- `stable`

## Guard Rules

- Tag format: `vX.Y.Z-(alpha|beta|rc).N`
- Stage transition must follow policy (`alpha -> beta -> rc -> stable`)
- No stage regression allowed for the same semantic version
- Same-stage tag numbers must increase monotonically (for example `alpha.1 -> alpha.2`)
- Tag commit must be reachable from `origin/main`
- `Cargo.toml` version at tag must match tag version

## Stage Gate Matrix

| Stage | Required previous stage | Required checks |
| --- | --- | --- |
| `alpha` | - | `CI Required Gate`, `Security Audit` |
| `beta` | `alpha` | `CI Required Gate`, `Security Audit`, `Feature Matrix Summary` |
| `rc` | `beta` | `CI Required Gate`, `Security Audit`, `Feature Matrix Summary`, `Nightly Summary & Routing` |
| `stable` | `rc` | `CI Required Gate`, `Security Audit`, `Feature Matrix Summary`, `Verify Artifact Set`, `Nightly Summary & Routing` |

The guard validates that the policy file defines this matrix shape completely. Missing or malformed matrix configuration fails validation.

## Transition Audit Trail

`prerelease-guard.json` now includes structured transition evidence:

- `transition.type`: `initial_stage`, `stage_iteration`, `promotion`, or `demotion_blocked`
- `transition.outcome`: final decision (`promotion`, `promotion_blocked`, `demotion_blocked`, etc.)
- `transition.previous_highest_stage` / `transition.previous_highest_tag`
- `transition.required_previous_stage` / `transition.required_previous_tag`

Demotion attempts are rejected and recorded as `demotion_blocked`.

## Release Stage History Publication

Guard artifacts publish release-stage history for the semantic version being validated:

- `stage_history.per_stage`: tags grouped by `alpha|beta|rc|stable`
- `stage_history.timeline`: normalized stage timeline entries
- `stage_history.latest_stage` / `stage_history.latest_tag`

The same history is rendered in `prerelease-guard.md` and appended to workflow summary.

## Outputs

- `prerelease-guard.json`
- `prerelease-guard.md`
- `audit-event-prerelease-guard.json`

## Publish Contract

- `dry-run`: guard + build + artifact manifest only
- `publish`: create/update GitHub prerelease and attach built assets
