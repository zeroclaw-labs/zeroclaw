# Game Feed

Card type: `game`

## Type-Safe Rules
- Emit `cardType: "game"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, league, teamA, teamB, scoreA, scoreB, status, detail

## Good Data Fits
- Live/finished fixtures, scoreboards, tournament brackets with concise status text.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
