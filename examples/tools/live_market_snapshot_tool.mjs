// Real-world registry tool handler example.
// Learning: return deterministic schema; avoid throwing raw upstream payloads.

export class LiveMarketSnapshotTool {
  static schema = {
    name: "live_market_snapshot",
    description: "Get a compact BTC/ETH market snapshot from live public APIs.",
    inputSchema: {
      type: "object",
      properties: {
        symbols: { type: "array", items: { type: "string" }, default: ["BTCUSDT", "ETHUSDT"] },
      },
    },
  };

  async handler(args = {}) {
    const symbols = Array.isArray(args.symbols) && args.symbols.length > 0 ? args.symbols : ["BTCUSDT", "ETHUSDT"];
    const url = `https://api.binance.com/api/v3/ticker/24hr?symbols=${encodeURIComponent(JSON.stringify(symbols))}`;

    const res = await fetch(url, { headers: { "User-Agent": "zeroclaw-examples/1.0" } });
    if (!res.ok) throw new Error(`upstream HTTP ${res.status}`);

    const rows = await res.json();
    const items = (Array.isArray(rows) ? rows : []).map((r) => ({
      symbol: String(r.symbol),
      price: Number(r.lastPrice),
      changePercent24h: Number(r.priceChangePercent),
      quoteVolume24h: Number(r.quoteVolume),
    }));

    return {
      success: true,
      output: JSON.stringify({ at: new Date().toISOString(), items }),
    };
  }
}
