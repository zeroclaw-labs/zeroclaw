# Code Feed

Card type: `code`

## Type-Safe Rules
- Emit `cardType: "code"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, language, code

## Good Data Fits
- Generated snippets, patch previews, diagnostics code blocks.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
