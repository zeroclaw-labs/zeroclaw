# Ci Feed

Card type: `ci`

## Type-Safe Rules
- Emit `cardType: "ci"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, repo, branch, commit, status, author, message, stages[]

## Good Data Fits
- Pipeline status, build/test/deploy stages, release gating.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
