import assert from 'node:assert/strict';
import test from 'node:test';
import React, { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { ChatMessage } from '@/contexts/AgentContext';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const values = new Map<string, string>([['zeroclaw_token', 'synthetic-paired-device']]);
const storage = { getItem: (k: string) => values.get(k) ?? null, setItem: (k: string, v: string) => values.set(k, v), removeItem: (k: string) => values.delete(k) };
let spoken: string[] = [];
let aborted = 0;
let capture: FakeRecognition;
let localStatus = 'available';
class FakeRecognition {
  static available = async () => localStatus;
  lang = ''; continuous = false; interimResults = false; processLocally = false;
  onresult: Function | null = null; onerror: Function | null = null; onend: Function | null = null;
  constructor() { capture = this; }
  start() {} stop() {} abort() { aborted++; }
}
class FakeUtterance { constructor(public text: string) {} }
const speech = Object.assign(new EventTarget(), {
  getVoices: () => [{ voiceURI: 'synthetic-local-voice', name: 'Test local voice', lang: 'en-US', localService: true }],
  cancel: () => {}, speak: (u: FakeUtterance) => spoken.push(u.text),
});
const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: 'https:', host: 'zc.example', hostname: 'zc.example', port: '', pathname: '/', search: '' },
  isSecureContext: true, SpeechRecognition: FakeRecognition, speechSynthesis: speech,
  setTimeout, clearTimeout,
});
const fakeDocument = Object.assign(new EventTarget(), { visibilityState: 'visible' });
Object.assign(globalThis, { React, window: fakeWindow, document: fakeDocument, localStorage: storage,
  SpeechSynthesisUtterance: FakeUtterance,
  MutationObserver: class { observe() {} disconnect() {} },
  IS_REACT_ACT_ENVIRONMENT: true,
});
const preferences = { dictation: true, reading: true, automatic: false, language: 'en-US', recognition: 'local', voice: 'synthetic-local-voice', rate: 1 };
const requests: { path: string; method: string; body: string | null }[] = [];
globalThis.fetch = async (input, init) => {
  requests.push({ path: String(input), method: init?.method ?? 'GET', body: typeof init?.body === 'string' ? init.body : null });
  return new Response(JSON.stringify({ device_id: 'synthetic-device', preferences }), { headers: { 'Content-Type': 'application/json' } });
};
const { default: Controls } = await import('./BrowserVoiceControls.tsx');
const { saveVoicePreferences } = await import('../lib/browserVoice.ts');
const reply = (id: string, content: string) => ({ id, content, role: 'agent' as const, timestamp: new Date(0) });
const options = { createNodeMock: () => ({ getClientRects: () => [1], parentElement: null }) };

async function mount(onTranscript = (_text: string) => {}) {
  spoken = []; aborted = 0; localStatus = 'available'; fakeDocument.visibilityState = 'visible';
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(createElement(Controls, { messages: [reply('old', 'Historical reply')], hydrated: true, typing: false, onTranscript }), options); });
  return renderer;
}

test('voice preferences use the paired-device API without writing browser preferences', async () => {
  const before = new Map(values);
  await saveVoicePreferences(preferences as Parameters<typeof saveVoicePreferences>[0]);
  assert.deepEqual(values, before);
  const sent = requests[requests.length - 1]!;
  assert.equal(sent.path, '/api/devices/me/voice'); assert.equal(sent.method, 'PUT');
  assert.equal(JSON.parse(sent.body!).device_id, undefined);
});

test('short press reads once; hold enables continuous; click disables without persisting', async () => {
  const renderer = await mount();
  assert.deepEqual(spoken, []);
  const speaker = () => renderer.root.findAllByType('button')[1]!;
  await act(async () => { speaker().props.onClick(); });
  assert.deepEqual(spoken, ['Historical reply']);
  await act(async () => { speaker().props.onClick(); });
  const writes = requests.filter(r => r.method === 'PUT').length;
  await act(async () => {
    speaker().props.onPointerDown({ button: 0, pointerId: 1, currentTarget: { setPointerCapture() {} } });
    await new Promise(resolve => setTimeout(resolve, 650));
  });
  await act(async () => { speaker().props.onPointerUp(); speaker().props.onClick(); });
  assert.equal(speaker().props['aria-pressed'], true);
  await act(async () => {
    renderer.update(createElement(Controls, { messages: [reply('old', 'Historical reply'), reply('new', 'New reply')], hydrated: true, typing: false, onTranscript() {} }));
  });
  assert.equal(spoken[spoken.length - 1], 'New reply');
  await act(async () => { speaker().props.onClick(); });
  assert.equal(speaker().props['aria-pressed'], false);
  assert.equal(requests.filter(r => r.method === 'PUT').length, writes);
  await act(async () => { renderer.unmount(); });
});

test('local dictation appends final text; hiding prevents stale results; unavailable local never starts capture', async () => {
  const transcripts: string[] = [];
  const renderer = await mount(text => transcripts.push(text));
  const mic = () => renderer.root.findAllByType('button')[0]!;
  await act(async () => { await mic().props.onClick(); });
  // The click handler starts asynchronously after capability detection.
  await act(async () => { await Promise.resolve(); });
  assert.equal(capture.processLocally, true);
  const stale = capture.onresult!;
  await act(async () => { stale({ resultIndex: 0, results: [{ isFinal: true, 0: { transcript: 'Draft only' } }] }); });
  assert.deepEqual(transcripts, ['Draft only']);
  await act(async () => { fakeDocument.visibilityState = 'hidden'; fakeDocument.dispatchEvent(new Event('visibilitychange')); });
  assert.equal(aborted, 1);
  stale({ resultIndex: 0, results: [{ isFinal: true, 0: { transcript: 'Discard this' } }] });
  assert.deepEqual(transcripts, ['Draft only']);
  fakeDocument.visibilityState = 'visible'; localStatus = 'unavailable';
  const prior = capture;
  await act(async () => { await mic().props.onClick(); await Promise.resolve(); });
  assert.equal(capture, prior);
  assert.equal(renderer.root.findAllByProps({ role: 'alert' }).length, 1);
  await act(async () => { renderer.unmount(); });
});

test('keyboard continuous mode reads only completed new assistant replies', async () => {
  const renderer = await mount();
  const speaker = () => renderer.root.findAllByType('button')[1]!;
  await act(async () => { speaker().props.onKeyDown({ key: 'Enter', shiftKey: true, preventDefault() {} }); });
  assert.equal(speaker().props['aria-pressed'], true);
  const messages: ChatMessage[] = [reply('old', 'Historical reply'),
    { ...reply('notice', 'Lifecycle notice'), notice: true },
    { ...reply('command', 'Local command output'), ephemeral: true },
    { ...reply('user', 'User message'), role: 'user' as const },
    { ...reply('tool', 'Tool output'), toolCall: { name: 'test', args: {}, output: 'Tool output' } },
    reply('final', 'Completed answer')];
  await act(async () => { renderer.update(createElement(Controls, { messages, hydrated: true, typing: true, onTranscript() {} })); });
  assert.deepEqual(spoken, []);
  await act(async () => { renderer.update(createElement(Controls, { messages, hydrated: true, typing: false, onTranscript() {} })); });
  assert.deepEqual(spoken, ['Completed answer']);
  await act(async () => { renderer.unmount(); });
});

test('configuration includes the usage guide and server-side device persistence explanation', async () => {
  const { default: Settings } = await import('./BrowserVoiceSettings.tsx');
  const html = renderToStaticMarkup(createElement(Settings));
  for (const text of ['How to use dictation and reading', 'click the microphone', 'press and hold', 'Shift+Enter', 'paired-device ID', 'never switches to an online service']) {
    assert.ok(html.includes(text), `missing help: ${text}`);
  }
});
