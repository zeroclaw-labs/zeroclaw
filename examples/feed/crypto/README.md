# Crypto Feed

Card type: `crypto`

## Type-Safe Rules
- Emit `cardType: "crypto"` only.
- Include all required metadata keys exactly as validated.
- Keep field types strict (numbers as numbers, int counters where required).
- Avoid fallback payloads and avoid optional-shape drift.

Required metadata fields:
- symbol, name, price, change24h, changePercent24h, volume24h, marketCap, sparkline[]

## Good Data Fits
- Exchange ticker/24h stats, chain-specific token dashboards, market breadth summaries.

## Rendering Goal
- Keep titles concise and metadata normalized so cards render crisply and consistently.
