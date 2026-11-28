// The voice silence timeout sends a dictated draft on its own. These tests mount the real chat
// page (AgentChat → ChatWorkspace → AgentProvider → BrowserVoiceControls) and check that text
// the person typed is never sent without a click. Faked: the browser speech API, the WebSocket
// and the gateway's HTTP API.
import assert from 'node:assert/strict';
import test from 'node:test';
import React, { createElement } from 'react';
import { act, create, type ReactTestInstance, type ReactTestRenderer } from 'react-test-renderer';

class FakeWebSocket {
  static readonly OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onclose: ((ev: { code: number }) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  sent: string[] = [];
  constructor(readonly url: string) { FakeWebSocket.instances.push(this); }
  send(data: string): void { this.sent.push(data); }
  close(): void { if (this.readyState === 3) return; this.readyState = 3; this.onclose?.({ code: 1000 }); }
}

let capture: FakeRecognition | undefined;
class FakeRecognition {
  static available = async () => 'available';
  static install = async () => true;
  lang = ''; continuous = false; interimResults = false; processLocally = false;
  onresult: Function | null = null; onerror: Function | null = null; onend: Function | null = null;
  onspeechstart: Function | null = null; onspeechend: Function | null = null;
  constructor() { capture = this; }
  start() {}
  stop() { this.onend?.(); }
  abort() {}
}
const values = new Map<string, string>([['zeroclaw_token', 'synthetic-paired-device']]);
const storage = { getItem: (k: string) => values.get(k) ?? null, setItem: (k: string, v: string) => void values.set(k, v), removeItem: (k: string) => void values.delete(k), clear: () => values.clear(), key: () => null, length: 0 };
const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: 'https:', host: 'zc.example', hostname: 'zc.example', port: '', pathname: '/', search: '', href: 'https://zc.example/' },
  isSecureContext: true, SpeechRecognition: FakeRecognition,
  speechSynthesis: Object.assign(new EventTarget(), { getVoices: () => [], cancel() {}, speak() {} }),
  setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms),
  clearTimeout: (id: ReturnType<typeof setTimeout>) => clearTimeout(id),
  matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
  localStorage: storage, sessionStorage: storage,
});
Object.assign(globalThis, {
  React, window: fakeWindow, document: Object.assign(new EventTarget(), { visibilityState: 'visible' }),
  localStorage: storage, sessionStorage: storage, WebSocket: FakeWebSocket,
  MutationObserver: class { observe() {} disconnect() {} },
  ResizeObserver: class { observe() {} unobserve() {} disconnect() {} },
  IS_REACT_ACT_ENVIRONMENT: true,
});

const preferences = { dictation: true, reading: false, automatic: false, language: 'en-US', recognition: 'local', voice: '', rate: 1 };
globalThis.fetch = async (input) => {
  const path = String(input).replace(/^https?:\/\/[^/]+/, '');
  const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
  if (path.startsWith('/api/config/prop')) return json({ path: 'gateway.voice_silence_timeout_secs', value: '15' });
  if (path === '/api/devices/me/voice') return json({ device_id: 'synthetic-device', preferences });
  return json({ error: 'not faked', path }, 404);
};

const { default: AgentChat } = await import('./AgentChat.tsx');
const { default: BrowserVoiceControls } = await import('../components/BrowserVoiceControls.tsx');
const { MemoryRouter, Route, Routes } = await import('react-router-dom');

const settle = () => act(async () => { await new Promise<void>(r => setTimeout(r, 20)); });
const sentMessages = () => FakeWebSocket.instances.flatMap(ws => ws.sent).filter(line => line.includes('"type":"message"'));

async function openChat(): Promise<ReactTestRenderer> {
  FakeWebSocket.instances.length = 0;
  values.clear(); values.set('zeroclaw_token', 'synthetic-paired-device');
  let r!: ReactTestRenderer;
  await act(async () => {
    r = create(createElement(MemoryRouter, { initialEntries: ['/agent/main'] },
      createElement(Routes, null, createElement(Route, { path: '/agent/:alias', element: createElement(AgentChat) }))),
      { createNodeMock: () => ({ getClientRects: () => [1], parentElement: null, focus() {}, scrollIntoView() {}, style: {}, scrollHeight: 0 }) });
  });
  await settle();
  for (const ws of FakeWebSocket.instances) { ws.readyState = 1; await act(async () => { ws.onopen?.(); }); }
  await settle();
  return r;
}

const composer = (r: ReactTestRenderer): ReactTestInstance => r.root.find(n => n.type === 'textarea');
const mic = (r: ReactTestRenderer): ReactTestInstance => r.root.findByType(BrowserVoiceControls).findAllByType('button')[0]!;
async function type(r: ReactTestRenderer, value: string) {
  await act(async () => { composer(r).props.onChange({ target: { value, style: {}, scrollHeight: 0 } }); });
}
async function dictate(text: string) {
  await act(async () => { capture!.onresult!({ resultIndex: 0, results: [{ isFinal: true, 0: { transcript: text } }] }); });
}

test('typing after the browser ends dictation early cancels the automatic send', async (context) => {
  const r = await openChat();
  context.mock.timers.enable({ apis: ['setTimeout'] });
  try {
    await act(async () => { await mic(r).props.onClick(); });
    await dictate('Deploy the staging build');
    // The browser ends recognition on its own after a short pause (mobile browsers do).
    await act(async () => { context.mock.timers.tick(2000); capture!.onend?.(); });
    // The person starts rewriting the message before the 15 s silence timeout.
    await type(r, 'Wait, do not deploy yet, first');
    await act(async () => { context.mock.timers.tick(30_000); });
    await act(async () => { await Promise.resolve(); });
    context.mock.timers.reset();
    await settle();
    assert.deepEqual(sentMessages(), [], 'the edited draft is not sent without a click');
    assert.equal(composer(r).props.value, 'Wait, do not deploy yet, first');
  } finally {
    context.mock.timers.reset();
    await act(async () => { r.unmount(); });
  }
});

test('typing while the microphone is live keeps the draft from being sent on silence', async (context) => {
  const r = await openChat();
  context.mock.timers.enable({ apis: ['setTimeout'] });
  try {
    await act(async () => { await mic(r).props.onClick(); });
    await dictate('Deploy the staging build');
    await type(r, 'Deploy the staging build after the');
    assert.equal(mic(r).props['aria-pressed'], true, 'typing does not turn the microphone off');
    await act(async () => { context.mock.timers.tick(15_000); });
    await act(async () => { await Promise.resolve(); });
    context.mock.timers.reset();
    await settle();
    assert.equal(mic(r).props['aria-pressed'], false, 'silence still ends the capture');
    assert.deepEqual(sentMessages(), []);
  } finally {
    context.mock.timers.reset();
    await act(async () => { r.unmount(); });
  }
});

test('with no edit, the silence timeout still sends the dictated text', async (context) => {
  const r = await openChat();
  context.mock.timers.enable({ apis: ['setTimeout'] });
  try {
    await act(async () => { await mic(r).props.onClick(); });
    await dictate('Deploy the staging build');
    await act(async () => { context.mock.timers.tick(2000); capture!.onend?.(); });
    await act(async () => { context.mock.timers.tick(12_999); });
    assert.deepEqual(sentMessages(), [], 'not sent before the 15 s timeout');
    await act(async () => { context.mock.timers.tick(1_000); });
    await act(async () => { await Promise.resolve(); });
    context.mock.timers.reset();
    await settle();
    const sent = sentMessages();
    assert.equal(sent.length, 1);
    assert.ok(sent[0]!.includes('"content":"Deploy the staging build"'), 'the dictated text is sent by the timer');
  } finally {
    context.mock.timers.reset();
    await act(async () => { r.unmount(); });
  }
});
