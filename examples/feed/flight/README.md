# Flight Feed

Card type: `flight`

## Type-Safe Rules
- Emit `cardType: "flight"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, flightNumber, airline, departure{}, arrival{}, status

## Good Data Fits
- Arrival/departure boards, tracked routes, logistics movement.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
