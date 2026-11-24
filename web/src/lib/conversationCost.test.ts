import assert from 'node:assert/strict';
import test from 'node:test';
import { ConversationCostPoller, conversationCostAmount, type CostScheduler } from './conversationCost.ts';
import type { ConversationCostSummary } from './api.ts';

const estimate: ConversationCostSummary = { estimated_cost_usd: 3.42, status: 'estimated', reasons: [] };
const flush = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };

function clock() {
  let pending: { delay: number; callback: () => void } | null = null;
  const schedule: CostScheduler = (callback, delay) => {
    const entry = { callback, delay };
    pending = entry;
    return () => { if (pending === entry) pending = null; };
  };
  return {
    schedule,
    delay: () => pending?.delay,
    fire: () => { const entry = pending; pending = null; entry?.callback(); },
  };
}

test('conversation polling coalesces requests and keeps refreshing after a turn', async () => {
  const timer = clock();
  let calls = 0;
  let resolve!: (value: ConversationCostSummary) => void;
  const seen: Array<ConversationCostSummary | null> = [];
  const poller = new ConversationCostPoller(() => { calls++; return new Promise((done) => { resolve = done; }); }, (value) => seen.push(value), () => false, timer.schedule);
  poller.setVisible(true);
  poller.refresh();
  poller.refresh();
  assert.equal(calls, 1);
  resolve(estimate);
  await flush();
  assert.deepEqual(seen, [estimate]);
  assert.equal(timer.delay(), 0);
  timer.fire();
  assert.equal(calls, 2);
  resolve({ ...estimate, estimated_cost_usd: 4 });
  await flush();
  assert.equal(timer.delay(), 2_000);
  poller.setVisible(false);
  assert.equal(timer.delay(), undefined);
  poller.setVisible(true);
  assert.equal(calls, 3);
  poller.dispose();
});

test('failed refresh clears the amount and uses capped backoff', async () => {
  const timer = clock();
  let fail = false;
  const seen: Array<ConversationCostSummary | null> = [];
  const poller = new ConversationCostPoller(async () => { if (fail) throw Error('offline'); return estimate; }, (value) => seen.push(value), () => false, timer.schedule);
  poller.setVisible(true);
  await flush();
  fail = true;
  for (const delay of [5_000, 10_000, 20_000, 30_000, 30_000]) {
    timer.fire();
    await flush();
    assert.equal(seen[seen.length - 1], null);
    assert.equal(timer.delay(), delay);
  }
  fail = false;
  timer.fire();
  await flush();
  assert.equal(timer.delay(), 2_000);
  poller.dispose();
});

test('unsupported or denied sessions stop until a new lifecycle is created', async () => {
  const timer = clock();
  let calls = 0;
  const poller = new ConversationCostPoller(async () => { calls++; throw Error('denied'); }, () => {}, () => true, timer.schedule);
  poller.setVisible(true);
  await flush();
  poller.refresh();
  poller.setVisible(false);
  poller.setVisible(true);
  assert.equal(calls, 1);
  assert.equal(timer.delay(), undefined);
  poller.dispose();
});

test('disposed conversation cannot publish a late result into another chat', async () => {
  let resolve!: (value: ConversationCostSummary) => void;
  let signal!: AbortSignal;
  const seen: Array<ConversationCostSummary | null> = [];
  const poller = new ConversationCostPoller((input) => { signal = input; return new Promise((done) => { resolve = done; }); }, (value) => seen.push(value), () => false);
  poller.setVisible(true);
  poller.dispose();
  assert.equal(signal.aborted, true);
  resolve(estimate);
  await flush();
  assert.deepEqual(seen, []);
});

test('formatting preserves true zero and tiny positive spending', () => {
  assert.equal(conversationCostAmount(0), '$0.0000');
  assert.equal(conversationCostAmount(0.0000001), '<$0.0001');
  assert.equal(conversationCostAmount(3.42), '$3.4200');
});
