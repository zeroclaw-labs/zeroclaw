import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CostBudget } from '@/types/api';
import { costBar, costOverviewBars, nextUtcDayStart, nextUtcMonthStart } from './costBudget.logic.ts';

const budget: CostBudget = {
  tracking: true,
  daily_limit_usd: 10,
  monthly_limit_usd: 100,
  warn_at_percent: 80,
};

test('no spend against a positive limit draws an empty bar', () => {
  const bars = costOverviewBars({ daily_cost_usd: 0, monthly_cost_usd: 0 }, budget);
  assert.equal(bars.daily?.percent, 0);
  assert.equal(bars.monthly?.percent, 0);
  assert.equal(bars.daily?.tone, 'normal');
});

test('the bars measure the day and month against their limits, whatever the window total', () => {
  const today = { session_cost_usd: 4, daily_cost_usd: 4, monthly_cost_usd: 55 };
  const allTime = { ...today, session_cost_usd: 240 };
  for (const cost of [today, allTime]) {
    const bars = costOverviewBars(cost, budget);
    assert.equal(bars.daily?.percent, 40);
    assert.equal(bars.monthly?.percent, 55);
    assert.equal(bars.daily?.limitUsd, 10);
    assert.equal(bars.monthly?.limitUsd, 100);
  }
});

test('the bar warns from the warning percentage and is full once the limit is reached', () => {
  assert.equal(costBar(7.99, 10, 80, true)?.tone, 'normal');
  assert.equal(costBar(8, 10, 80, true)?.tone, 'warning');
  assert.deepEqual(costBar(10, 10, 80, true), { percent: 100, tone: 'reached', limitUsd: 10 });
  assert.deepEqual(costBar(25, 10, 80, true), { percent: 100, tone: 'reached', limitUsd: 10 });
});

test('a zero limit is full as soon as anything is spent, a negative one from the start', () => {
  assert.deepEqual(costBar(0, 0, 80, true), { percent: 0, tone: 'normal', limitUsd: 0 });
  assert.deepEqual(costBar(0.01, 0, 80, true), { percent: 100, tone: 'reached', limitUsd: 0 });
  assert.deepEqual(costBar(0, -1, 80, true), { percent: 100, tone: 'reached', limitUsd: -1 });
});

test('no bar without tracking, without a limit, or without the budget fields', () => {
  assert.equal(costBar(4, 10, 80, false), null);
  assert.equal(costBar(4, null, 80, true), null);
  const unset = { ...budget, daily_limit_usd: null };
  const bars = costOverviewBars({ daily_cost_usd: 4, monthly_cost_usd: 55 }, unset);
  assert.equal(bars.daily, null);
  assert.equal(bars.monthly?.percent, 55);
  assert.deepEqual(costOverviewBars({ daily_cost_usd: 4, monthly_cost_usd: 55 }, null), {
    daily: null,
    monthly: null,
  });
});

test('the daily and monthly totals reset at the next UTC midnight and UTC month start', () => {
  const late = new Date('2026-11-21T23:59:59Z');
  assert.equal(nextUtcDayStart(late).toISOString(), '2026-11-22T00:00:00.000Z');
  assert.equal(nextUtcMonthStart(late).toISOString(), '2026-12-01T00:00:00.000Z');
  const yearEnd = new Date('2026-12-31T12:00:00Z');
  assert.equal(nextUtcDayStart(yearEnd).toISOString(), '2027-01-01T00:00:00.000Z');
  assert.equal(nextUtcMonthStart(yearEnd).toISOString(), '2027-01-01T00:00:00.000Z');
});
