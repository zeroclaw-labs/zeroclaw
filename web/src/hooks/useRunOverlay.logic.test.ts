import assert from 'node:assert/strict';
import test from 'node:test';

import { LatestOverlayWriteGate } from './useRunOverlay.logic.ts';

test('accepted cancellation supersedes a poll that started earlier', () => {
  const gate = new LatestOverlayWriteGate();
  const stalePoll = gate.beginRequest();

  gate.supersedePendingRequests();

  assert.equal(gate.isCurrent(stalePoll), false);
});

test('only the newest overlapping poll may update the overlay', () => {
  const gate = new LatestOverlayWriteGate();
  const olderPoll = gate.beginRequest();
  const newerPoll = gate.beginRequest();

  assert.equal(gate.isCurrent(olderPoll), false);
  assert.equal(gate.isCurrent(newerPoll), true);
});
