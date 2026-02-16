# Social Feed

Card type: `social`

## Type-Safe Rules
- Emit `cardType: "social"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, handle, displayName, content, likes(int), reposts(int), timestamp, platform

## Good Data Fits
- Community posts, creator updates, forum messages with engagement counters.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
