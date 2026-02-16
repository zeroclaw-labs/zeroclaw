# Table Feed

Card type: `table`

## Type-Safe Rules
- Emit `cardType: "table"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, columns[], rows[]

## Good Data Fits
- Rankings, leaderboards, record listings, compact query results.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
