# Integration Feed

Card type: `integration`

## Type-Safe Rules
- Emit `cardType: "integration"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, name, type, status, lastSync, metrics[]

## Good Data Fits
- SaaS sync health, connector status, API quota/state cards.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
