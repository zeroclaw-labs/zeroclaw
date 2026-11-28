import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CodexUsageChecker,
  formatPercent,
  parseCodexUsage,
  type CodexUsageOk,
} from './codexUsage.logic.ts';

const ok = (route: string, five = 10): CodexUsageOk => ({
  status: 'ok',
  route,
  checked_at: '2027-01-15T08:00:00Z',
  five_hour: { used_percent: five, resets_at: '2027-01-15T12:00:00Z' },
  weekly: null,
});
const ready = { connected: true, switching: false };

function deferred() {
  let resolve!: (value: unknown) => void;
  const promise = new Promise<unknown>((done) => { resolve = done; });
  return { promise, resolve };
}

test('only an explicit check on a ready socket requests usage, without an agent or route argument', async () => {
  const calls: unknown[][] = [];
  const checker = new CodexUsageChecker(async (...args) => {
    calls.push(args);
    return ok('openai.override');
  });
  checker.reset();
  assert.equal(checker.state.kind, 'idle');
  await checker.check({ ...ready, connected: false });
  assert.deepEqual(checker.state, { kind: 'problem', problem: 'route_unavailable' });
  await checker.check({ ...ready, switching: true });
  assert.deepEqual(checker.state, { kind: 'problem', problem: 'switching' });
  assert.deepEqual(calls, []);
  await checker.check(ready);
  assert.deepEqual(calls, [[]]);
  assert.deepEqual(checker.state, { kind: 'ok', usage: ok('openai.override') });
});

test('only the latest check wins, even if replies arrive out of order', async () => {
  const first = deferred();
  const second = deferred();
  const queue = [first.promise, second.promise];
  const checker = new CodexUsageChecker(() => queue.shift()!);
  const p1 = checker.check(ready);
  const p2 = checker.check(ready);
  second.resolve(ok('openai.override', 2));
  await p2;
  first.resolve(ok('openai.configured', 77));
  await p1;
  assert.deepEqual(checker.state, { kind: 'ok', usage: ok('openai.override', 2) });
});

test('refresh discards prior account values immediately, including real zero, and keeps none on failure', async () => {
  const refresh = deferred();
  const replies = [
    async () => ok('openai.same-alias', 0),
    () => refresh.promise,
    async () => ({ status: 'unavailable', reason: 'not_permitted' }),
    async () => ({ status: 'unavailable', reason: 'unrecognized synthetic text' }),
    async () => { throw new Error('transport'); },
    async () => ({ nonsense: true }),
  ];
  const checker = new CodexUsageChecker(() => replies.shift()!());
  await checker.check(ready);
  assert.deepEqual(checker.state, { kind: 'ok', usage: ok('openai.same-alias', 0) });
  const pending = checker.check(ready);
  assert.deepEqual(checker.state, { kind: 'loading' });
  refresh.resolve({ status: 'unavailable', reason: 'not_signed_in' });
  await pending;
  assert.deepEqual(checker.state, { kind: 'problem', problem: 'not_signed_in' });
  for (const problem of ['not_permitted', 'unavailable', 'request_failed', 'request_failed']) {
    await checker.check(ready);
    assert.deepEqual(checker.state, { kind: 'problem', problem });
  }
});

test('a reset discards an in-flight response without scheduling another request', async () => {
  const response = deferred();
  let calls = 0;
  const checker = new CodexUsageChecker(() => { calls += 1; return response.promise; });
  const pending = checker.check(ready);
  checker.reset();
  response.resolve(ok('openai.old'));
  await pending;
  assert.deepEqual(checker.state, { kind: 'idle' });
  assert.equal(calls, 1);
});

test('the parser keeps unreported windows and fields distinct from real zero, and strips extras', () => {
  const value = ok('openai.codex', 0);
  assert.deepEqual(parseCodexUsage({
    ...value,
    five_hour: { ...value.five_hour, extra: 'synthetic account detail' },
  }), value);
  const absent = { ...value, five_hour: { used_percent: null, resets_at: null } };
  assert.deepEqual(parseCodexUsage(absent), absent);
  assert.equal(parseCodexUsage({ ...value, five_hour: null }), null);
  assert.equal(parseCodexUsage({ ...value, weekly: undefined }), null);
  assert.equal(parseCodexUsage({ ...value, five_hour: { used_percent: -1, resets_at: null } }), null);
  assert.equal(parseCodexUsage('x'), null);
});

test('formatPercent preserves zero and formats non-integral observations', () => {
  assert.equal(formatPercent(0), '0');
  assert.equal(formatPercent(12), '12');
  assert.equal(formatPercent(12.34), '12.3');
});
