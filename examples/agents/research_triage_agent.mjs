// Agent example: combine tools and memory for lightweight triage.
// Learning: keep tool calls explicit and narrow to reduce ambiguity.

export class ResearchTriageAgent {
  static config = {
    name: "research_triage",
    description: "Summarize fresh inputs and produce actionable next steps.",
    tools: ["live_market_snapshot", "web_search"],
    memory: "short_term",
  };

  async run(ctx) {
    const prompt = String(ctx?.input ?? "");

    const market = await ctx.tools.live_market_snapshot({ symbols: ["BTCUSDT", "ETHUSDT"] });
    const references = await ctx.tools.web_search({ query: prompt, limit: 5 });

    return {
      summary: "Triage complete",
      decisions: [
        "Prioritize high-signal references",
        "Track BTC/ETH volatility for context",
      ],
      market,
      references,
    };
  }
}
