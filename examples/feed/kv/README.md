# Kv Feed

Card type: `kv`

## Type-Safe Rules
- Emit `cardType: "kv"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, pairs[]

## Good Data Fits
- System health snapshots, config values, counters and gauges.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
