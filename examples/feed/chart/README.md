# Chart Feed

Card type: `chart`

## Type-Safe Rules
- Emit `cardType: "chart"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, chartType, data[]

## Good Data Fits
- Time-series metrics, distributions, trend lines, categorical comparisons.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
