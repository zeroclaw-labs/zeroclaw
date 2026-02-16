# Prediction Feed

Card type: `prediction`

## Type-Safe Rules
- Emit `cardType: "prediction"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, question, yesPrice, volume, category, endDate, source

## Good Data Fits
- Prediction market contracts, derived probability models, event likelihood tracking.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
