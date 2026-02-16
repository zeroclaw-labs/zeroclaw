# Webview Feed

Card type: `webview`

## Type-Safe Rules
- Emit `cardType: "webview"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- id, title, url

## Good Data Fits
- Embedded dashboards, docs pages, internal reports.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
