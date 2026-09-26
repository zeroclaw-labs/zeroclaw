import assert from 'node:assert/strict';
import test from 'node:test';

import { resolveContextBarState } from './contextBar.ts';

test('uses model window as denominator and marks the trim budget', () => {
  const state = resolveContextBarState(180_000, 200_000, 100_000);
  assert.ok(state);
  assert.equal(state.denominator, 200_000);
  assert.equal(state.percent, 50);
  assert.equal(state.trimMarkerIndex, 14);
  assert.equal(state.cells[14], '│');
});

test('falls back to the legacy absolute budget when capacity is absent', () => {
  const state = resolveContextBarState(32_000, null, 16_000);
  assert.ok(state);
  assert.equal(state.denominator, 32_000);
  assert.equal(state.percent, 50);
  assert.equal(state.trimMarkerIndex, null);
});

test('zero disable sentinel does not fabricate a denominator', () => {
  assert.equal(resolveContextBarState(0, null, 100), null);
  const state = resolveContextBarState(0, 8_000, 100);
  assert.ok(state);
  assert.equal(state.denominator, 8_000);
  assert.equal(state.trimMarkerIndex, null);
});
