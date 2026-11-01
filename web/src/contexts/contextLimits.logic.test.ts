import assert from 'node:assert/strict';
import test from 'node:test';

import { resolveContextBarState } from '../lib/contextBar.ts';
import {
  applyHistoryTrimToDisplayedBudget,
  contextLimitsFromDoneFrame,
} from './contextLimits.logic.ts';

test('a done frame without capacity clears the previous route denominator', () => {
  let limits = contextLimitsFromDoneFrame({
    max_context_tokens: 180_000,
    model_context_window: 200_000,
  });
  assert.deepEqual(limits, { maxTokens: 180_000, modelWindow: 200_000 });

  limits = contextLimitsFromDoneFrame({
    max_context_tokens: 32_000,
  });
  assert.deepEqual(limits, { maxTokens: 32_000, modelWindow: null });

  const rendered = resolveContextBarState(
    limits.maxTokens,
    limits.modelWindow,
    16_000,
  );
  assert.ok(rendered);
  assert.equal(rendered.denominator, 32_000);
  assert.equal(rendered.trimMarkerIndex, null);
});

test('a trim event replaces the displayed budget with the post-trim snapshot', () => {
  const current = { inputTokens: 180_000, maxTokens: 160_000 };
  const trimmed = applyHistoryTrimToDisplayedBudget(current, {
    tokens_after: 90_000,
    token_budget: 128_000,
  });
  assert.deepEqual(trimmed, { inputTokens: 90_000, maxTokens: 128_000 });

  const rendered = resolveContextBarState(trimmed.maxTokens, 200_000, trimmed.inputTokens);
  assert.ok(rendered);
  assert.equal(rendered.used, 90_000);
  assert.equal(rendered.denominator, 200_000);
});

test('a message-limit trim leaves the displayed budget alone', () => {
  const current = { inputTokens: 12_000, maxTokens: 32_000 };
  assert.deepEqual(applyHistoryTrimToDisplayedBudget(current, {}), current);
});
