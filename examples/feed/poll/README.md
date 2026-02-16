# Poll Feed

Card type: `poll`

## Type-Safe Rules
- Emit `cardType: "poll"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, question, options[]>=2, totalVotes(int)

## Good Data Fits
- Live votes, A/B preference snapshots, community sentiment options.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
