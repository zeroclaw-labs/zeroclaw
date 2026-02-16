# File Feed

Card type: `file`

## Type-Safe Rules
- Emit `cardType: "file"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, name, extension, contentType, size(int), createdAt, updatedAt, description, tags

## Good Data Fits
- Artifacts, reports, docs, exports with durable metadata.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
