export interface ContextBarState {
  used: number;
  denominator: number;
  percent: number;
  cells: string;
  trimMarkerIndex: number | null;
}

/** Build the provider-route-aware state shared by the context meter UI. */
export function resolveContextBarState(
  contextMaxTokens: number | null,
  contextModelWindow: number | null,
  contextInputTokens: number | null,
  barWidth = 16,
): ContextBarState | null {
  const denominator = contextModelWindow ?? contextMaxTokens;
  if (!denominator || denominator <= 0 || barWidth <= 0) return null;

  const used = contextInputTokens ?? 0;
  const percent = Math.min((used / denominator) * 100, 100);
  const filled = Math.round((percent / 100) * barWidth);
  const cells = ('█'.repeat(filled) + '░'.repeat(Math.max(0, barWidth - filled))).split('');
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
    cells[trimMarkerIndex] = '│';
  }

  return { used, denominator, percent, cells: cells.join(''), trimMarkerIndex };
}
