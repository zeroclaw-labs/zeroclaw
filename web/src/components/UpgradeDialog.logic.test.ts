import assert from 'node:assert/strict';
import test from 'node:test';

import { canAutoRestart, upgradeBlockedMessageKey } from './UpgradeDialog.logic.ts';

test('desktop-supervised restarts are auto-restartable', () => {
  assert.equal(canAutoRestart('desktop_supervised'), true);
  assert.equal(canAutoRestart('supervised'), true);
  assert.equal(canAutoRestart('self_respawn'), true);
});

test('manual and unknown restart modes require operator action', () => {
  assert.equal(canAutoRestart('manual'), false);
  assert.equal(canAutoRestart(undefined), false);
});

test('a desktop-bundled kernel points at the desktop app, even when self-upgrade is enabled', () => {
  assert.equal(upgradeBlockedMessageKey(true, true), 'upgrade.desktop_bundled');
  assert.equal(upgradeBlockedMessageKey(false, true), 'upgrade.desktop_bundled');
});

test('a separately installed kernel follows gateway.allow_self_upgrade', () => {
  assert.equal(upgradeBlockedMessageKey(false, false), 'upgrade.disabled');
  assert.equal(upgradeBlockedMessageKey(true, false), null);
});
