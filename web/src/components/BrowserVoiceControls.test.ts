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
  static install = async (options: { langs: string[]; processLocally?: boolean }) => {
    assert.deepEqual(options, { langs: ['en-US'], processLocally: true });
    localStatus = 'available';
    return true;
  };
  lang = ''; continuous = false; interimResults = false; processLocally = false;
  onresult: Function | null = null; onerror: Function | null = null; onend: Function | null = null;
  constructor() { capture = this; }
  start() {}
  stop() {
    this.onresult?.({ resultIndex: 0, results: [{ isFinal: true, 0: { transcript: 'Final after stop' } }] });
    this.onend?.();
  }
  abort() { aborted++; }
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
const otherPreferences = { ...preferences, dictation: false, reading: false, voice: 'other-device-voice', language: 'pt-BR', rate: 1.3 };
const silentPreferences = { ...preferences, dictation: false, reading: false, voice: '' };
const perDevice = new Map([['synthetic-device', preferences], ['other-device', otherPreferences], ['silent-device', silentPreferences]]);
const requests: { path: string; method: string; body: string | null }[] = [];
globalThis.fetch = async (input, init) => {
  const path = String(input);
  requests.push({ path, method: init?.method ?? 'GET', body: typeof init?.body === 'string' ? init.body : null });
  if (path === '/api/devices') return new Response(JSON.stringify({
    current_device_id: 'synthetic-device',
    devices: [
      { id: 'synthetic-device', name: 'Example browser A', device_type: 'browser' },
      { id: 'other-device', name: 'Example browser B', device_type: 'browser' },
      { id: 'silent-device', name: 'Example silent browser', device_type: 'browser' },
    ],
  }), { headers: { 'Content-Type': 'application/json' } });
  const deviceId = path === '/api/devices/me/voice' ? 'synthetic-device' : decodeURIComponent(path.split('/')[3] ?? '');
  const selected = perDevice.get(deviceId);
  if (!selected) return new Response('', { status: 404 });
  if (init?.method === 'PUT') {
    const incoming = JSON.parse(String(init.body)) as typeof preferences;
    selected.dictation = incoming.dictation;
    selected.reading = incoming.reading;
    selected.automatic = incoming.automatic;
    selected.language = incoming.language;
    selected.recognition = incoming.recognition;
    selected.voice = incoming.voice;
    selected.rate = incoming.rate;
  }
  return new Response(JSON.stringify({ device_id: deviceId, preferences: selected }), { headers: { 'Content-Type': 'application/json' } });
};
const { default: Controls } = await import('./BrowserVoiceControls.tsx');
const { saveVoicePreferences, useVoicePreferences } = await import('../lib/browserVoice.ts');
const { mapServerMessagesToPersisted, persistedToUiMessages } = await import('../lib/chatHistoryStorage.ts');
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
  assert.match(speaker().findByType('svg').props.className, /lucide-volume-x/);
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

test('clicking the active microphone keeps the final transcript emitted on stop', async () => {
  const transcripts: string[] = [];
  const renderer = await mount(text => transcripts.push(text));
  const mic = () => renderer.root.findAllByType('button')[0]!;
  const icon = () => mic().findByType('svg').props.className;
  assert.match(icon(), /lucide-mic-off/);
  await act(async () => { await mic().props.onClick(); });
  assert.match(icon(), /lucide-mic(?: |$)/);
  await act(async () => { await mic().props.onClick(); });
  assert.match(icon(), /lucide-mic-off/);
  assert.deepEqual(transcripts, ['Final after stop']);
  assert.equal(aborted, 0);
  assert.equal(mic().props['aria-pressed'], false);
  await act(async () => { renderer.unmount(); });
});

test('sending cancels capture and discards late text for the submitted draft', async () => {
  const captureRef = React.createRef<() => void>();
  const transcripts: string[] = [];
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(createElement(Controls, { messages: [], hydrated: true, typing: false,
      onTranscript: text => transcripts.push(text), captureRef }), options);
  });
  const mic = () => renderer.root.findAllByType('button')[0]!;
  await act(async () => { await mic().props.onClick(); });
  assert.equal(mic().props['aria-pressed'], true);
  const late = capture.onresult!;
  await act(async () => { captureRef.current?.(); });
  late({ resultIndex: 0, results: [{ isFinal: true, 0: { transcript: 'Late text' } }] });
  assert.deepEqual(transcripts, []);
  assert.equal(mic().props['aria-pressed'], false);
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

test('the language download explicitly requests local recognition', async () => {
  localStatus = 'downloadable';
  const { default: Settings } = await import('./BrowserVoiceSettings.tsx');
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(createElement(Settings), options); });
  const install = renderer.root.findAllByType('button')
    .find(button => button.props.children === 'Download local language pack');
  assert.ok(install);
  await act(async () => { await install.props.onClick(); });
  assert.equal(localStatus, 'available');
  await act(async () => { renderer.unmount(); });
});

test('configuration includes the usage guide and server-side device persistence explanation', async () => {
  const { default: Settings } = await import('./BrowserVoiceSettings.tsx');
  const html = renderToStaticMarkup(createElement(Settings));
  for (const text of ['How to use dictation and reading', 'click the microphone', 'press and hold', 'Shift+Enter', 'paired-device ID', 'never switches to an online service']) {
    assert.ok(html.includes(text), `missing help: ${text}`);
  }
});

test('a trim notice is silent and the reply waits for an active microphone to stop', async () => {
  const renderer = await mount();
  const button = (index: number) => renderer.root.findAllByType('button')[index]!;
  await act(async () => {
    button(1).props.onKeyDown({ key: 'Enter', shiftKey: true, preventDefault() {} });
    await button(0).props.onClick();
  });
  const messages = [
    reply('old', 'Historical reply'),
    { ...reply('trim', 'Earlier conversation history was trimmed'), notice: true, ephemeral: true },
    reply('answer', 'Two plus two is four.'),
  ];
  await act(async () => {
    renderer.update(createElement(Controls, { messages, hydrated: true, typing: false, onTranscript() {} }));
  });
  assert.deepEqual(spoken, []);
  await act(async () => { await button(0).props.onClick(); });
  assert.deepEqual(spoken, ['Two plus two is four.']);
  await act(async () => { renderer.unmount(); });
});

test('reloaded history with regenerated IDs is never treated as a live reply', async () => {
  const renderer = await mount();
  const speaker = () => renderer.root.findAllByType('button')[1]!;
  await act(async () => { speaker().props.onKeyDown({ key: 'Enter', shiftKey: true, preventDefault() {} }); });
  const rows = [{ role: 'assistant' as const, content: 'Historical reply', created_at: null }];
  const restored = persistedToUiMessages(mapServerMessagesToPersisted(rows));
  await act(async () => { renderer.update(createElement(Controls, { messages: restored, hydrated: true, typing: false, onTranscript() {} })); });
  assert.deepEqual(spoken, []);
  await act(async () => { renderer.update(createElement(Controls, { messages: [...restored, reply('live', 'New completed reply')], hydrated: true, typing: false, onTranscript() {} })); });
  assert.deepEqual(spoken, ['New completed reply']);
  const again = persistedToUiMessages(mapServerMessagesToPersisted([...rows, { role: 'assistant', content: 'New completed reply', created_at: null }]));
  await act(async () => { renderer.update(createElement(Controls, { messages: again, hydrated: true, typing: false, onTranscript() {} })); });
  assert.deepEqual(spoken, ['New completed reply']);
  await act(async () => { renderer.unmount(); });
});

test('returning to a tab refetches server preferences and disables stale reading choices', async () => {
  const original = { ...preferences };
  const renderer = await mount();
  try {
    preferences.reading = false;
    preferences.recognition = 'browser';
    fakeDocument.visibilityState = 'hidden';
    await act(async () => { fakeDocument.dispatchEvent(new Event('visibilitychange')); });
    fakeDocument.visibilityState = 'visible';
    await act(async () => { fakeDocument.dispatchEvent(new Event('visibilitychange')); });
    assert.equal(renderer.root.findAllByType('button').length, 1);
    await act(async () => { renderer.root.findByType('button').props.onClick(); });
    assert.equal(capture.processLocally, false);
    preferences.dictation = false;
    await act(async () => { fakeWindow.dispatchEvent(new Event('focus')); });
    assert.equal(renderer.root.findAllByType('button').length, 0);
  } finally {
    await act(async () => { renderer.unmount(); });
    Object.assign(preferences, original);
  }
});

test('an old resume fetch cannot overwrite a newer saved preference', async () => {
  const original = { ...preferences };
  const originalFetch = globalThis.fetch;
  function Probe() { return createElement('output', null, JSON.stringify(useVoicePreferences())); }
  let renderer!: ReactTestRenderer;
  await act(async () => { renderer = create(createElement(Probe)); });
  let resolve!: (response: Response) => void;
  const pending = new Promise<Response>(done => { resolve = done; });
  let delay = true;
  globalThis.fetch = async (input, init) => {
    if (delay && (!init?.method || init.method === 'GET')) { delay = false; return pending; }
    return originalFetch(input, init);
  };
  try {
    await act(async () => { fakeWindow.dispatchEvent(new Event('focus')); });
    await act(async () => { await saveVoicePreferences({ ...preferences, recognition: 'local', reading: false }); });
    await act(async () => { resolve(new Response(JSON.stringify({ device_id: 'synthetic-device', preferences: original }), { headers: { 'Content-Type': 'application/json' } })); });
    const state = JSON.parse(String(renderer.root.findByType('output').children[0]));
    assert.equal(state.preferences.reading, false);
    assert.equal(state.ready, true);
  } finally {
    globalThis.fetch = originalFetch;
    await act(async () => { renderer.unmount(); });
    Object.assign(preferences, original);
  }
});

test('cross-tab messages invalidate server state without carrying preference authority', async () => {
  const original = { ...preferences };
  const stored = new Map(values);
  const sent: unknown[] = [];
  const channels: Notifications[] = [];
  class Notifications {
    onmessage: ((event: { data: unknown }) => void) | null = null;
    closed = false;
    constructor(_topic: string) { channels.push(this); }
    postMessage(data: unknown) { sent.push(data); }
    close() { this.closed = true; }
  }
  Object.assign(fakeWindow, { BroadcastChannel: Notifications });
  function Probe() { return createElement('output', null, JSON.stringify(useVoicePreferences())); }
  let renderer!: ReactTestRenderer;
  try {
    await act(async () => { renderer = create(createElement(Probe)); });
    preferences.reading = false;
    await act(async () => { channels[0]!.onmessage!({ data: { device_id: 'synthetic-device', preferences: { reading: true } } }); });
    assert.equal(JSON.parse(String(renderer.root.findByType('output').children[0])).preferences.reading, false);
    await act(async () => { await saveVoicePreferences({ ...preferences, recognition: 'local', reading: true }); });
    assert.deepEqual(sent, [{ device_id: 'synthetic-device' }]);
    assert.deepEqual(values, stored);
  } finally {
    if (renderer) await act(async () => { renderer.unmount(); });
    Reflect.deleteProperty(fakeWindow, 'BroadcastChannel');
    Object.assign(preferences, original);
  }
  assert.ok(channels.every(channel => channel.closed));
});

test('device list loads and edits the selected record without changing this browser', async () => {
  const ownBefore = { ...preferences };
  const otherBefore = { ...otherPreferences };
  const { default: Settings } = await import('./BrowserVoiceSettings.tsx');
  let renderer!: ReactTestRenderer;
  const reading = () => renderer.root.findAllByType('label').find(label => label.children.some(child => typeof child === 'string' && child.includes('Show speaker / enable reading')))!.findByType('input');
  try {
    await act(async () => { renderer = create(createElement(Settings)); });
    const list = () => renderer.root.findByProps({ 'aria-label': 'Paired device' });
    assert.equal(list().props.value, 'synthetic-device');
    assert.equal(list().findAllByType('option').length, 3);
    await act(async () => { list().props.onChange({ target: { value: 'other-device' } }); });
    assert.equal(reading().props.checked, false);
    assert.ok(JSON.stringify(renderer.toJSON()).includes('other-device-voice'));
    await act(async () => { reading().props.onChange({ target: { checked: true } }); });
    assert.equal(otherPreferences.reading, true);
    assert.deepEqual(preferences, ownBefore);
    const write = requests.filter(request => request.method === 'PUT').slice(-1)[0]!;
    assert.equal(write.path, '/api/devices/other-device/voice');
    await act(async () => { list().props.onChange({ target: { value: 'silent-device' } }); });
    assert.equal(reading().props.checked, false);
    await act(async () => { list().props.onChange({ target: { value: 'synthetic-device' } }); });
    assert.equal(reading().props.checked, ownBefore.reading);
  } finally {
    if (renderer) await act(async () => { renderer.unmount(); });
    Object.assign(preferences, ownBefore); Object.assign(otherPreferences, otherBefore);
  }
});

test('a late selected-device response cannot repaint a different device configuration', async () => {
  const originalFetch = globalThis.fetch;
  let resolve!: (response: Response) => void;
  const pending = new Promise<Response>(done => { resolve = done; });
  globalThis.fetch = async (input, init) => String(input) === '/api/devices/other-device/voice' ? pending : originalFetch(input, init);
  function Probe({ id }: { id: string }) { return createElement('output', null, JSON.stringify(useVoicePreferences(id))); }
  let renderer!: ReactTestRenderer;
  try {
    await act(async () => { renderer = create(createElement(Probe, { id: 'synthetic-device' })); });
    await act(async () => { renderer.update(createElement(Probe, { id: 'other-device' })); });
    await act(async () => { renderer.update(createElement(Probe, { id: 'silent-device' })); });
    await act(async () => { resolve(new Response(JSON.stringify({ device_id: 'other-device', preferences: otherPreferences }), { headers: { 'Content-Type': 'application/json' } })); });
    const state = JSON.parse(String(renderer.root.findByType('output').children[0]));
    assert.equal(state.deviceId, 'silent-device');
    assert.equal(state.preferences.voice, '');
    assert.equal(state.preferences.reading, false);
  } finally {
    if (renderer) await act(async () => { renderer.unmount(); });
    globalThis.fetch = originalFetch;
  }
});
