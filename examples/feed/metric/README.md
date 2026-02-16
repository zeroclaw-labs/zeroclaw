# Metric Feed

Card type: `metric`

## Type-Safe Rules
- Emit `cardType: "metric"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, label, value

## Good Data Fits
- Single KPI cards: latency, revenue, queue depth, conversion.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
