# Audio Feed

Card type: `audio`

## Type-Safe Rules
- Emit `cardType: "audio"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, url

## Good Data Fits
- Podcast episodes, alert audio clips, stream links.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
