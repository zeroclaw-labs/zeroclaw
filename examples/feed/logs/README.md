# Logs Feed

Card type: `logs`

## Type-Safe Rules
- Emit `cardType: "logs"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, entries[]

## Good Data Fits
- Error/event streams, deploy logs, status incident timelines.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
