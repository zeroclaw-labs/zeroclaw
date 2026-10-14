import assert from 'node:assert/strict';
import test from 'node:test';

import { confirmsCancellation, runsStreamEffect } from './runs.logic.ts';

test('a lagged run stream requires a fresh authoritative snapshot', () => {
  assert.equal(runsStreamEffect('lagged'), 'resync');
});

test('authoritative requested or terminal state clears a stale request error', () => {
  assert.equal(confirmsCancellation('running', false), false);
  assert.equal(confirmsCancellation('cancel_requested', false), true);
  assert.equal(confirmsCancellation('cancelled', true), true);
});

test('ordinary run frames retain their incremental update behavior', () => {
  assert.equal(runsStreamEffect('snapshot'), 'replace');
  assert.equal(runsStreamEffect('run'), 'upsert');
  assert.equal(runsStreamEffect('disabled'), 'disable');
  assert.equal(runsStreamEffect('error'), 'ignore');
});
