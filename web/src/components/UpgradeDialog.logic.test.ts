import assert from 'node:assert/strict';
import test from 'node:test';

import { canAutoRestart } from './UpgradeDialog.logic.ts';

test('desktop-supervised restarts are auto-restartable', () => {
  assert.equal(canAutoRestart('desktop_supervised'), true);
  assert.equal(canAutoRestart('supervised'), true);
  assert.equal(canAutoRestart('self_respawn'), true);
});

test('manual and unknown restart modes require operator action', () => {
  assert.equal(canAutoRestart('manual'), false);
  assert.equal(canAutoRestart(undefined), false);
});
