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
let modelPickerProperty: Promise<unknown> | undefined;
globalThis.fetch = async (input) => {
  const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
  let body: unknown;
  if (url.includes('/api/config/catalog')) body = { providers: [] };
  else if (url.includes('/api/status')) body = { model: 'fixture-model' };
  else if (url.includes('/api/config/prop')) body = await (modelPickerProperty ?? { path: '', value: '<unset>' });
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
  usage: (sessionId: string) => Promise<unknown> = async () => null;
  checkCodexUsage(sessionId: string): Promise<unknown> { return this.usage(sessionId); }
  onMessage: ((msg: WsMessage) => void) | null = null;
  onOpen: (() => void) | null = null;
  onClose: ((ev: CloseEvent) => void) | null = null;
  onError: ((ev: Event) => void) | null = null;
  connected = false;
  sentMessages = 0;
  connect(): void { this.connected = true; this.onOpen?.(); }
  disconnect(): void { this.connected = false; }
  sendMessage(): void { this.sentMessages += 1; }
  sendApprovalResponse(): void {}
}

async function mountMeter() {
  localStorage.clear();
  localStorage.setItem('zeroclaw_active_session.ops', 'meter-session');
  const socket = new MeterSocket();
  const runtime: AgentSessionRuntime = {
    createSocket: () => socket,
    getMessages: async (sessionId) => ({ session_id: sessionId, session_persistence: true, messages: [] }),
    getState: async (sessionId) => ({ session_id: sessionId, state: 'idle' }),
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
    socket,
    text: () => container.textContent ?? '',
    async click(label: string) {
      const button = [...container.querySelectorAll('button')].find((b) => b.textContent?.trim() === label);
      assert.ok(button, `button ${label} is rendered`);
      await act(async () => { button.click(); });
    },
    async refreshModels() { await act(async () => { current?.refreshModels(); }); },
    async receive(frame: WsMessage) {
      await act(async () => { socket.onMessage?.(frame); });
    },
    async receiveBatch(frames: WsMessage[]) {
      await act(async () => { for (const frame of frames) socket.onMessage?.(frame); });
    },
    async switchSession() {
      await act(async () => { assert.equal(current?.goToSession('next-meter-session'), true); });
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

test('actual web meter labels derived counts and clears provenance with measured or absent usage', async () => {
  const mounted = await mountMeter();
  try {
    await mounted.receive(done());
    const measured = mounted.meter();
    assert.ok(measured && !measured.includes('~'), measured ?? 'missing meter');
    for (const source of ['calibrated', 'estimate']) {
      await mounted.receive(trim({ tokens_after_source: source }));
      const derived = mounted.meter();
      assert.ok(derived, 'derived meter is present');
      assert.ok(derived.includes('~ 90,000 / 200,000'), derived);
      assert.ok(derived.includes('~45%'), derived);
      assert.equal(derived.length, measured.length, 'source marker reuses the measured separator cells');
      await mounted.receive(trim({ dropped_messages: 0, tokens_after_source: 'provider' }));
      assert.equal(mounted.meter(), derived);
      await mounted.receive(trim({ tokens_after: undefined, tokens_after_source: 'provider' }));
      assert.equal(mounted.meter(), derived);
      await mounted.receive(done({ context_input_tokens: 90_000, context_input_tokens_source: source }));
      assert.equal(mounted.meter(), derived, 'done preserves both derived count and source');
      await mounted.receive(done({ last_input_tokens: 90_000 }));
      assert.ok(!mounted.meter()?.includes('~'), 'same count with measured provenance clears the marker');
    }
    await mounted.receiveBatch([done(), trim()]);
    assert.ok(mounted.meter()?.includes('~45%'), 'batched frames retain count and source together');
    await mounted.receive(done({ last_input_tokens: null }));
    assertCount(mounted.meter(), 0);
    assert.ok(!mounted.meter()?.includes('~'), 'absent usage clears provenance');
    await mounted.receive(trim());
    await mounted.switchSession();
    assertCount(mounted.meter(), 0);
    assert.ok(!mounted.meter()?.includes('~'), 'session reset clears provenance with count');
  } finally { await mounted.unmount(); }
});

// Recorded from websocket_terminal_trim_survives_done_without_changing_usage_or_next_turn.
// Override with a fresh capture to prove both ends on the same revision.
const capturedFrames = process.env.ZEROCLAW_CONTEXT_METER_TEST_FRAMES_PATH
  ?? new URL('./fixtures/context-meter-ws-frames.json', import.meta.url);
test('real gateway terminal frames reach the actual web meter unchanged', async () => {
  const frames = JSON.parse(await readFile(capturedFrames, 'utf8')) as WsMessage[];
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
    assert.equal(terminal.context_input_tokens_source, lastTrim.tokens_after_source);
    const denominator = terminal.model_context_window ?? terminal.max_context_tokens;
    assert.ok(denominator);
    const approximatePercent = `~${(100 * lastTrim.tokens_after / denominator).toFixed(0)}%`;
    assert.ok(mounted.meter()?.includes(approximatePercent), 'captured calibrated count remains approximate');
  } finally { await mounted.unmount(); }
});

test.after(async () => { await dom.happyDOM.close(); });

function usageFrame(percent: number) {
  return {
    status: 'ok', route: 'openai.fixture-override', checked_at: '2027-01-15T08:00:00Z',
    five_hour: { used_percent: percent, resets_at: null }, weekly: null,
  };
}

test('Codex usage displays the server-selected route before the first message', async () => {
  const chat = await mountMeter();
  try {
    let checks = 0;
    chat.socket.usage = async (sessionId) => {
      assert.equal(sessionId, 'meter-session');
      checks += 1;
      return { ...usageFrame(37), route: 'openai.fixture' };
    };
    await chat.receive({ type: 'session_start', session_id: 'meter-session', message_count: 0 });
    await chat.click('Codex usage');
    assert.equal(checks, 0, 'opening the panel must not poll');
    await chat.click('Check Codex usage');
    assert.match(chat.text(), /Chat route: openai\.fixture/);
    assert.match(chat.text(), /37% used/);
    assert.equal(checks, 1);
    assert.equal(chat.socket.sentMessages, 0, 'checking usage does not send a message');
  } finally {
    await chat.unmount();
  }
});

test('Loading the model picker label does not clear usage for the same socket', async () => {
  let loaded!: (property: unknown) => void;
  modelPickerProperty = new Promise((resolve) => { loaded = resolve; });
  const chat = await mountMeter();
  try {
    assert.doesNotMatch(chat.text(), /openai\.picker-label/);
    chat.socket.usage = async () => usageFrame(37);
    await chat.click('Codex usage');
    await chat.click('Check Codex usage');
    assert.match(chat.text(), /37% used/);
    await act(async () => { loaded({ path: '', value: 'openai.picker-label' }); });
    assert.match(chat.text(), /openai\.picker-label/);
    assert.match(chat.text(), /37% used/);
    assert.match(chat.text(), /Chat route: openai\.fixture-override/);
  } finally {
    modelPickerProperty = undefined;
    await chat.unmount();
  }
});

test('Codex usage clears on session change and an ambiguous model refresh; late replies stay hidden', async () => {
  const chat = await mountMeter();
  try {
    await chat.click('Codex usage');
    assert.doesNotMatch(chat.text(), /% used/);
    chat.socket.usage = async (sessionId) => {
      assert.equal(sessionId, 'meter-session');
      return usageFrame(42);
    };
    await chat.click('Check Codex usage');
    assert.match(chat.text(), /Chat route: openai\.fixture-override/);
    assert.match(chat.text(), /42% used/);
    await chat.refreshModels();
    assert.doesNotMatch(chat.text(), /42% used/);

    let deliver!: (usage: unknown) => void;
    chat.socket.usage = (sessionId) => {
      assert.equal(sessionId, 'meter-session');
      return new Promise((resolve) => { deliver = resolve; });
    };
    await chat.click('Check Codex usage');
    await chat.switchSession();
    await act(async () => { deliver(usageFrame(87)); });
    assert.doesNotMatch(chat.text(), /87% used/);

    chat.socket.usage = async (sessionId) => {
      assert.equal(sessionId, 'next-meter-session');
      return usageFrame(3);
    };
    await chat.click('Check Codex usage');
    assert.match(chat.text(), /3% used/);
  } finally {
    await chat.unmount();
  }
});

test('Codex usage clears on same-tab credential changes and hides on loss of operator access', async () => {
  const { setToken, setOidcSession } = await import('../lib/auth.ts');
  const chat = await mountMeter();
  try {
    await act(async () => { setToken('synthetic-pairing-one'); });
    await chat.click('Codex usage');
    chat.socket.usage = async () => usageFrame(29);
    await chat.click('Check Codex usage');
    assert.match(chat.text(), /29% used/);
    await act(async () => { setToken('synthetic-pairing-two'); });
    assert.doesNotMatch(chat.text(), /29% used/);

    let deliver!: (usage: unknown) => void;
    chat.socket.usage = () => new Promise((resolve) => { deliver = resolve; });
    await chat.click('Check Codex usage');
    await act(async () => {
      setOidcSession({
        kind: 'oidc', provider: 'oidc.fixture', principal_id: 'scoped', display_name: 'Scoped',
        admin: false, expires_at: 2_000_000_000, csrf_token: 'synthetic',
        features: { chat: true, operator_pages: false },
      });
      deliver(usageFrame(73));
    });
    assert.doesNotMatch(chat.text(), /Codex usage|73% used/);
  } finally {
    await chat.unmount();
    setOidcSession(null);
  }
});
