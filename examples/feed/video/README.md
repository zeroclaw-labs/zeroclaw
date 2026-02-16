# Video Feed

Card type: `video`

## Type-Safe Rules
- Emit `cardType: "video"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, url

## Good Data Fits
- Channel uploads, lecture updates, release trailers.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
