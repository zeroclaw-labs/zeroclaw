export const CONTEXT_BAR_WIDTH = 16;

export interface ContextBarState {
  used: number;
  denominator: number;
  percent: number;
  /** Number of filled cells in a {@link CONTEXT_BAR_WIDTH}-cell bar. */
  filled: number;
  trimMarkerIndex: number | null;
}

/** Build the provider-route-aware state shared by the context meter UI. */
export function resolveContextBarState(
  contextMaxTokens: number | null,
  contextModelWindow: number | null,
  contextInputTokens: number | null,
  barWidth = CONTEXT_BAR_WIDTH,
): ContextBarState | null {
  const denominator = contextModelWindow ?? contextMaxTokens;
  if (!denominator || denominator <= 0 || barWidth <= 0) return null;

  const used = contextInputTokens ?? 0;
  const percent = Math.min((used / denominator) * 100, 100);
  const filled = Math.round((percent / 100) * barWidth);
  let trimMarkerIndex: number | null = null;
  if (
    contextModelWindow &&
    contextMaxTokens !== null &&
    contextMaxTokens > 0 &&
    contextMaxTokens < contextModelWindow
  ) {
    trimMarkerIndex = Math.min(
      Math.round((contextMaxTokens / contextModelWindow) * barWidth),
      barWidth - 1,
    );
  }

  return { used, denominator, percent, filled, trimMarkerIndex };
}
