# Image Feed

Card type: `image`

## Type-Safe Rules
- Emit `cardType: "image"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, url

## Good Data Fits
- APOD/photo feeds, product images, camera snapshots.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
