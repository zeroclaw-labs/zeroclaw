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

test('a real trim replaces only the count and preserves the canonical budget', () => {
  const current = { inputTokens: 180_000, maxTokens: 160_000 };
  const trimmed = applyHistoryTrimToDisplayedBudget(current, {
    tokens_after: 90_000,
    dropped_messages: 4,
  });
  assert.deepEqual(trimmed, { inputTokens: 90_000, maxTokens: 160_000 });

  const rendered = resolveContextBarState(trimmed.maxTokens, 200_000, trimmed.inputTokens);
  assert.ok(rendered);
  assert.equal(rendered.used, 90_000);
  assert.equal(rendered.denominator, 200_000);
});

test('a message-limit trim leaves the displayed budget alone', () => {
  const current = { inputTokens: 12_000, maxTokens: 32_000 };
  assert.deepEqual(applyHistoryTrimToDisplayedBudget(current, {}), current);
});


test('a zero-drop floor notice cannot replace the displayed count', () => {
  const current = { inputTokens: 12_000, maxTokens: 8_000 };
  assert.equal(applyHistoryTrimToDisplayedBudget(current, {
    dropped_messages: 0,
    tokens_after: 4_000,
  }), current);
});

test('real trims cannot enable a disabled or unknown budget', () => {
  for (const maxTokens of [0, null]) {
    assert.deepEqual(applyHistoryTrimToDisplayedBudget(
      { inputTokens: 12_000, maxTokens },
      { dropped_messages: 2, tokens_after: 6_000 },
    ), { inputTokens: 6_000, maxTokens });
  }
});
