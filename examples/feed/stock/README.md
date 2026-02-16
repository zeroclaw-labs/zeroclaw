# Stock Feed

Card type: `stock`

## Type-Safe Rules
- Emit `cardType: "stock"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- ticker, name, price, change, changePercent, sparkline[]

## Good Data Fits
- Public equity quotes, ETF snapshots, top movers, intraday candles compressed into sparkline.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
