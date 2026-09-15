import assert from 'node:assert/strict';
import test from 'node:test';

import { resolveContextBarState } from '../lib/contextBar.ts';
import { contextLimitsFromDoneFrame } from './contextLimits.logic.ts';

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
