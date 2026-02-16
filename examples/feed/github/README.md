# Github Feed

Card type: `github`

## Type-Safe Rules
- Emit `cardType: "github"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, repo, events[]

## Good Data Fits
- Repo activity streams: PRs, issues, pushes, comments.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
