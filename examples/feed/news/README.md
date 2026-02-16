# News Feed

Card type: `news`

## Type-Safe Rules
- Emit `cardType: "news"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, headline, source, category, timestamp

## Good Data Fits
- Headlines from wires/blog feeds, product changelogs, incident advisories.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
