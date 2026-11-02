import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import React, { act, createElement } from 'react';
import { Window } from 'happy-dom';
import type { AgentContextValue, AgentSessionRuntime, SessionSocket } from './AgentContext.tsx';
import type { WsMessage } from '../types/api.ts';

// Mount the actual chat page in a DOM: this exercises the socket receiver,
// context provider, and final meter element rather than a helper snapshot.
const dom = new Window({ url: 'http://localhost/' });
for (const key of [
  'window', 'document', 'navigator', 'localStorage', 'HTMLElement', 'HTMLInputElement',
  'HTMLTextAreaElement', 'Node', 'Element', 'Event', 'MouseEvent', 'KeyboardEvent',
  'Text', 'MutationObserver', 'ResizeObserver', 'DOMRect', 'Range',
] as const) {
  Object.defineProperty(globalThis, key, {
    configurable: true, writable: true,
    value: key === 'window' ? dom : dom[key as keyof Window],
  });
}
Object.assign(globalThis, {
  React,
  IS_REACT_ACT_ENVIRONMENT: true,
  getComputedStyle: dom.getComputedStyle.bind(dom),
  requestAnimationFrame: dom.requestAnimationFrame.bind(dom),
  cancelAnimationFrame: dom.cancelAnimationFrame.bind(dom),
});
globalThis.fetch = async (input) => {
  const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
  let body: unknown;
  if (url.includes('/api/config/catalog')) body = { providers: [] };
  else if (url.includes('/api/status')) body = { model: 'fixture-model' };
  else if (url.includes('/api/config/prop')) body = { path: '', value: '<unset>' };
  else if (url.includes('/api/config/list')) body = { entries: [] };
  else if (url.endsWith('/api/sessions')) body = { sessions: [] };
  else return new Response('{"error":"not found"}', { status: 404 });
  return new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
};

const { createRoot } = await import('react-dom/client');
const { MemoryRouter } = await import('react-router-dom');
const { AgentProvider, useAgent } = await import('./AgentContext.tsx');
const { AgentChatInner } = await import('../pages/AgentChat.tsx');
const { DraftContext } = await import('../hooks/useDraft.ts');

class MeterSocket implements SessionSocket {
  onMessage: ((msg: WsMessage) => void) | null = null;
  onOpen: (() => void) | null = null;
  onClose: ((ev: CloseEvent) => void) | null = null;
  onError: ((ev: Event) => void) | null = null;
  connected = false;
  connect(): void { this.connected = true; this.onOpen?.(); }
  disconnect(): void { this.connected = false; }
  sendMessage(): void {}
  sendApprovalResponse(): void {}
}

async function mountMeter() {
  localStorage.clear();
  localStorage.setItem('zeroclaw_active_session.ops', 'meter-session');
  const socket = new MeterSocket();
  const runtime: AgentSessionRuntime = {
    createSocket: () => socket,
    getMessages: async (sessionId) => ({ session_id: sessionId, session_persistence: true, messages: [] }),
    delete: async () => ({ deleted: true }),
    rename: async (sessionId, name) => ({ session_id: sessionId, name }),
    mintId: () => 'meter-session',
  };
  const drafts = new Map<string, string>();
  let current: AgentContextValue | undefined;
  function Probe() { current = useAgent(); return null; }
  const container = document.createElement('div');
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(createElement(MemoryRouter, null,
      createElement(AgentProvider, {
        agentAlias: 'ops', sessionRuntime: runtime,
        children: createElement(React.Fragment, null,
          createElement(Probe),
          createElement(DraftContext.Provider, { value: {
            getDraft: (key: string) => drafts.get(key) ?? '',
            setDraft: (key: string, value: string) => { drafts.set(key, value); },
            clearDraft: (key: string) => { drafts.delete(key); },
          } }, createElement(AgentChatInner, { agentAlias: 'ops' }))),
      })));
  });
  assert.ok(socket.onMessage, 'the real AgentProvider installs the receiving callback');
  assert.ok(current?.hydrated, 'chat hydration completes before events are delivered');
  return {
    async receive(frame: WsMessage) {
      await act(async () => { socket.onMessage?.(frame); });
    },
    meter(): string | null {
      // AgentChat's visible meter is the span containing its token bar.
      return [...container.querySelectorAll('span')]
        .map((span) => span.textContent ?? '')
        .find((text) => text.includes(' / ') && text.includes('[') && text.includes('%')) ?? null;
    },
    async unmount() { await act(async () => { root.unmount(); }); container.remove(); },
  };
}

function done(overrides: Partial<WsMessage> = {}): WsMessage {
  return {
    type: 'done', full_response: 'fixture answer', last_input_tokens: 180_000,
    input_tokens: 180_000, max_context_tokens: 160_000, model_context_window: 200_000,
    ...overrides,
  };
}
function trim(overrides: Partial<WsMessage> = {}): WsMessage {
  return {
    type: 'history_trimmed', dropped_messages: 4, dropped_turns: 1, kept_turns: 1,
    reason: 'context token budget exceeded', token_budget: 200_000,
    tokens_before: 180_000, tokens_after: 90_000, tokens_after_source: 'calibrated',
    unsatisfiable_floor: true, ...overrides,
  };
}
function assertCount(text: string | null, count: number, denominator = 200_000) {
  assert.ok(text, 'the actual chat meter is present');
  assert.ok(text.includes(`${count.toLocaleString().padStart(7)} / ${denominator.toLocaleString().padStart(7)}`), text);
}

// One mounted page receives each sequence so stale state and final-frame
// overwrites are observable at the user's actual meter.
test('actual web meter retains a real terminal trim and canonical limits', async () => {
  const mounted = await mountMeter();
  try {
    await mounted.receive(done());
    assertCount(mounted.meter(), 180_000);
    const before = mounted.meter();
    await mounted.receive(trim({ dropped_messages: 0 }));
    assert.equal(mounted.meter(), before, 'a zero-drop floor notice must not change the meter');
    await mounted.receive(trim({ tokens_after: undefined }));
    assert.equal(mounted.meter(), before, 'message-only trimming has no count');

    await mounted.receive(trim());
    assertCount(mounted.meter(), 90_000);
    assert.ok(mounted.meter()?.includes('│'), 'the emergency event threshold cannot erase the canonical trim marker');
    const after = mounted.meter();
    await mounted.receive(done({ context_input_tokens: 90_000, context_input_tokens_source: 'calibrated' }));
    assert.equal(mounted.meter(), after, 'done cannot restore the old measured prompt');

    await mounted.receive(done({ last_input_tokens: 70_000 }));
    assertCount(mounted.meter(), 70_000);
    await mounted.receive(done({ last_input_tokens: null }));
    assertCount(mounted.meter(), 0);
    await mounted.receive(done({ last_input_tokens: undefined, input_tokens: 42_000 }));
    assertCount(mounted.meter(), 42_000);
    await mounted.receive(done({ last_input_tokens: undefined, input_tokens: undefined }));
    assertCount(mounted.meter(), 42_000);
    await mounted.receive(done({ last_input_tokens: null, context_input_tokens: 6_000 }));
    assertCount(mounted.meter(), 6_000);

    // With no configured capacity, disabled/unknown budgets remain hidden.
    for (const max_context_tokens of [0, undefined]) {
      await mounted.receive(done({ max_context_tokens, model_context_window: undefined }));
      assert.equal(mounted.meter(), null);
      await mounted.receive(trim());
      assert.equal(mounted.meter(), null, 'a trim must not invent a configured denominator');
    }
    // Known capacity remains visible when proactive trimming is disabled.
    await mounted.receive(done({ max_context_tokens: 0 }));
    await mounted.receive(trim());
    assertCount(mounted.meter(), 90_000);
    assert.ok(!mounted.meter()?.includes('│'), 'disabled trimming has no threshold marker');
  } finally { await mounted.unmount(); }
});

test('actual web meter does not restore pre-trim usage on done', async () => {
  const mounted = await mountMeter();
  try {
    await mounted.receive(done());
    await mounted.receive(trim({ token_budget: 160_000, unsatisfiable_floor: undefined }));
    assertCount(mounted.meter(), 90_000);
    await mounted.receive(done({ context_input_tokens: 90_000, context_input_tokens_source: 'calibrated' }));
    assertCount(mounted.meter(), 90_000);
  } finally { await mounted.unmount(); }
});

const capturedFrames = process.env.ZEROCLAW_CONTEXT_METER_TEST_FRAMES_PATH;
test('real gateway terminal frames reach the actual web meter unchanged', {
  skip: capturedFrames ? false : 'set ZEROCLAW_CONTEXT_METER_TEST_FRAMES_PATH to the real WS regression capture',
}, async () => {
  const frames = JSON.parse(await readFile(capturedFrames!, 'utf8')) as WsMessage[];
  const terminal = [...frames].reverse().find((frame) => frame.type === 'done');
  const lastTrim = [...frames].reverse().find((frame) => frame.type === 'history_trimmed');
  assert.ok(terminal && lastTrim && typeof lastTrim.tokens_after === 'number');
  assert.notEqual(terminal.last_input_tokens, lastTrim.tokens_after);
  const mounted = await mountMeter();
  try {
    // The first completed turn provides the canonical route snapshot. The
    // captured trim/done sequence then updates the same mounted chat page.
    await mounted.receive(done({ ...terminal, context_input_tokens: undefined, context_input_tokens_source: undefined }));
    for (const frame of frames) await mounted.receive(frame);
    assertCount(mounted.meter(), lastTrim.tokens_after, terminal.model_context_window ?? terminal.max_context_tokens);
    assert.equal(terminal.context_input_tokens, lastTrim.tokens_after);
  } finally { await mounted.unmount(); }
});

test.after(async () => { await dom.happyDOM.close(); });
