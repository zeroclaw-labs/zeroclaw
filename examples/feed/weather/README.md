# Weather Feed

Card type: `weather`

## Type-Safe Rules
- Emit `cardType: "weather"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, location, temp, feelsLike, condition, humidity, wind, forecast[]

## Good Data Fits
- Current conditions, short forecasts, city/location climate snapshots.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
