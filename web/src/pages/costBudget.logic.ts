// Bars for the dashboard's Cost Overview: each period's spend against its
// configured limit.
import type { CostBudget } from '@/types/api';

/** `normal` below the warning threshold, `warning` from it, `reached` once
 * spend has reached the limit (the budget check refuses any spend that would
 * go past it). */
export type CostBarTone = 'normal' | 'warning' | 'reached';

export interface CostBar {
  /** Bar width, 0 to 100. */
  percent: number;
  tone: CostBarTone;
  limitUsd: number;
}

/**
 * The bar for one period's spend, or null when there is nothing to measure it
 * against: cost tracking is off or unavailable, or the period has no limit.
 * A zero or negative limit allows no spend (the budget check refuses any
 * spend that would go over it), so any spend fills it, and a negative one is
 * full from the start.
 */
export function costBar(
  spentUsd: number,
  limitUsd: number | null,
  warnAtPercent: number,
  tracking: boolean,
): CostBar | null {
  if (!tracking || limitUsd === null || !Number.isFinite(limitUsd)) return null;
  const spent = Number.isFinite(spentUsd) && spentUsd > 0 ? spentUsd : 0;
  // Multiply before dividing so round amounts give exact percentages.
  const used = limitUsd > 0 ? (spent * 100) / limitUsd : spent > limitUsd ? Infinity : 0;
  const tone: CostBarTone =
    used >= 100 ? 'reached' : used >= warnAtPercent ? 'warning' : 'normal';
  return { percent: Math.min(used, 100), tone, limitUsd };
}

/**
 * The Daily and Monthly bars. They read the UTC day and month totals, which
 * don't depend on the window selected on the Cost tab.
 */
export function costOverviewBars(
  cost: { daily_cost_usd: number; monthly_cost_usd: number },
  budget: CostBudget | null,
): { daily: CostBar | null; monthly: CostBar | null } {
  if (!budget) return { daily: null, monthly: null };
  const warn = budget.warn_at_percent;
  return {
    daily: costBar(cost.daily_cost_usd, budget.daily_limit_usd, warn, budget.tracking),
    monthly: costBar(cost.monthly_cost_usd, budget.monthly_limit_usd, warn, budget.tracking),
  };
}
