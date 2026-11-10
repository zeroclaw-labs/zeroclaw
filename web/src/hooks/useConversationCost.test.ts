import assert from 'node:assert/strict';
import test from 'node:test';
import React, { createElement } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ConversationCostSummary } from '../lib/api.ts';

const windowEvents = new EventTarget();
const documentEvents = Object.assign(new EventTarget(), { visibilityState: 'visible' });
Object.assign(globalThis, {
  React, window: windowEvents, document: documentEvents,
  localStorage: { getItem: () => null }, IS_REACT_ACT_ENVIRONMENT: true,
});
Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { language: 'en' } });
const { useConversationCost } = await import('./useConversationCost.ts');
const { HttpError, UnauthorizedError } = await import('../lib/api.ts');
const { AUTH_EVENTS } = await import('../lib/auth.ts');
const summary: ConversationCostSummary = { estimated_cost_usd: 1.0, status: 'estimated', reasons: [] };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test('cost hook discards old sessions, disconnects, and authentication generations', async () => {
  const requests: Array<{ id: string; signal?: AbortSignal } & ReturnType<typeof deferred<ConversationCostSummary>>> = [];
  let value: ConversationCostSummary | null = null;
  const load = (id: string, signal?: AbortSignal) => {
    const request = { id, signal, ...deferred<ConversationCostSummary>() };
    requests.push(request);
    return request.promise;
  };
  function Probe({ id, connected = true }: { id: string; connected?: boolean }) {
    value = useConversationCost(id, connected, false, load);
    return null;
  }
  let root!: ReactTestRenderer;
  await act(async () => { root = create(createElement(Probe, { id: 'a' })); });
  await act(async () => { root.update(createElement(Probe, { id: 'b' })); });
  assert.equal(requests[0]!.signal?.aborted, true);
  await act(async () => { requests[0]!.resolve(summary); });
  assert.equal(value, null);
  await act(async () => { requests[1]!.resolve(summary); });
  assert.deepEqual(value, summary);
  await act(async () => { windowEvents.dispatchEvent(new Event(AUTH_EVENTS.accessLost)); });
  assert.equal(value, null);
  await act(async () => { root.update(createElement(Probe, { id: 'c' })); });
  assert.equal(requests.length, 2);
  await act(async () => { windowEvents.dispatchEvent(new Event(AUTH_EVENTS.resumed)); });
  assert.equal(requests.length, 3);
  await act(async () => { root.update(createElement(Probe, { id: 'c', connected: false })); });
  await act(async () => { requests[2]!.resolve(summary); });
  assert.equal(value, null);
  await act(async () => { root.update(createElement(Probe, { id: 'c' })); });
  assert.equal(requests.length, 4);
  await act(async () => { root.unmount(); });
});

test('cost hook stops unsupported or denied polling across views until its reset boundary', async () => {
  for (const denied of [false, true]) {
    let calls = 0;
    const load = async () => { calls += 1; throw denied ? new UnauthorizedError() : new HttpError(404, 'not found'); };
    function Probe({ id, connected = true }: { id: string; connected?: boolean }) {
      useConversationCost(id, connected, false, load);
      return null;
    }
    let root!: ReactTestRenderer;
    await act(async () => { root = create(createElement(Probe, { id: 'a' })); });
    await act(async () => { root.update(createElement(Probe, { id: 'b' })); });
    assert.equal(calls, 1);
    await act(async () => { windowEvents.dispatchEvent(new Event(AUTH_EVENTS.resumed)); });
    assert.equal(calls, denied ? 2 : 1);
    await act(async () => { root.update(createElement(Probe, { id: 'b', connected: false })); });
    await act(async () => { root.update(createElement(Probe, { id: 'b' })); });
    assert.equal(calls, 2);
    if (denied) {
      await act(async () => { windowEvents.dispatchEvent(new Event(AUTH_EVENTS.resumed)); });
      assert.equal(calls, 3);
    }
    await act(async () => { root.unmount(); });
  }
});


test('cost hook pauses both hidden workspace panes and hidden browser documents', async () => {
  let calls = 0;
  const load = async () => { calls += 1; return summary; };
  function Probe({ visible }: { visible: boolean }) {
    useConversationCost('a', true, false, load, visible);
    return null;
  }
  let root!: ReactTestRenderer;
  await act(async () => { root = create(createElement(Probe, { visible: false })); });
  assert.equal(calls, 0);
  await act(async () => { root.update(createElement(Probe, { visible: true })); });
  assert.equal(calls, 1);
  await act(async () => {
    documentEvents.visibilityState = 'hidden';
    documentEvents.dispatchEvent(new Event('visibilitychange'));
    root.update(createElement(Probe, { visible: false }));
  });
  await act(async () => { root.update(createElement(Probe, { visible: true })); });
  assert.equal(calls, 1);
  await act(async () => {
    documentEvents.visibilityState = 'visible';
    documentEvents.dispatchEvent(new Event('visibilitychange'));
  });
  assert.equal(calls, 2);
  await act(async () => { root.unmount(); });
});
