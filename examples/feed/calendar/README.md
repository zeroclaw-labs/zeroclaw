# Calendar Feed

Card type: `calendar`

## Type-Safe Rules
- Emit `cardType: "calendar"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, date, events[]

## Good Data Fits
- Meetings, release calendars, holidays, launch timelines.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
