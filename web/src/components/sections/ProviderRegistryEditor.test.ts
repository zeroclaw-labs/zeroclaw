import assert from 'node:assert/strict';
import test from 'node:test';
import React, { createElement } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { MemoryRouter } from 'react-router-dom';

const location = { protocol: 'https:', host: 'zc.example', hostname: 'zc.example', port: '', pathname: '/config/providers.registry', search: '' };
Object.assign(globalThis, {
  React, location,
  window: Object.assign(new EventTarget(), { location }),
  IS_REACT_ACT_ENVIRONMENT: true,
});
const { default: Editor } = await import('./ProviderRegistryEditor.tsx');
const { useModelProfileAliases } = await import('../../lib/useModelProfileAliases.ts');
const { configuredResourceIds } = await import('../../lib/configuredModels.ts');
const original = {
  connections: { 'gateway.prod': { kind: 'custom', auth: { mode: 'api_key', api_key: '********' } } },
  models: { 'chat.fast.us': { capability: 'chat', provider: 'gateway.prod', model: 'before' } },
  model_presets: {}, fallback: {},
};

test('registry editor saves exact keys and the original masked baseline atomically', async () => {
  let current = structuredClone(original);
  let saved: { expected: typeof original; registry: typeof original } | undefined;
  let saves = 0;
  globalThis.fetch = async (_input, init) => {
    if (init?.method === 'PUT') {
      saved = JSON.parse(init.body as string);
      current = saved!.registry;
    }
    return new Response(JSON.stringify(init?.method === 'PUT' ? { saved: true } : current), { headers: { 'Content-Type': 'application/json' } });
  };
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => {
      renderer = create(createElement(MemoryRouter, { initialEntries: ['/config/providers.registry?model=chat.fast.us'] }, createElement(Editor, { onSaved: () => { saves++; } })));
    });
    const textarea = renderer!.root.findByType('textarea');
    assert.match(textarea.props.value, /chat\.fast\.us/);
    await act(async () => textarea.props.onChange({ target: { value: JSON.stringify({ 'chat.fast.us': { ...original.models['chat.fast.us'], model: 'after' } }) } }));
    const save = renderer!.root.findAllByType('button').find((button) => String(button.props.children).includes('Save'))!;
    await act(async () => { save.props.onClick(); });
    assert.deepEqual(saved?.expected, original);
    assert.equal(saved?.registry.models['chat.fast.us'].model, 'after');
    assert.deepEqual(saved?.registry.connections, original.connections);
    assert.equal(saves, 1);
  } finally { await act(async () => renderer?.unmount()); }
});

test('invalid registry JSON stays in the editor and never sends a write', async () => {
  let writes = 0;
  globalThis.fetch = async (_input, init) => {
    if (init?.method === 'PUT') writes++;
    return new Response(JSON.stringify(original), { headers: { 'Content-Type': 'application/json' } });
  };
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => { renderer = create(createElement(MemoryRouter, null, createElement(Editor, { onSaved: () => assert.fail('invalid JSON must not save') }))); });
    const textarea = renderer!.root.findByType('textarea');
    await act(async () => textarea.props.onChange({ target: { value: '{invalid' } }));
    const save = renderer!.root.findAllByType('button').find((button) => String(button.props.children).includes('Save'))!;
    await act(async () => { save.props.onClick(); });
    assert.equal(writes, 0);
    assert.equal(renderer!.root.findByProps({ role: 'alert' }).children.length > 0, true);
  } finally { await act(async () => renderer?.unmount()); }
});

test('canonical references take precedence over legacy family aliases', async () => {
  const requests: string[] = [];
  let registry = original as { models: Record<string, unknown> };
  globalThis.fetch = async (input) => {
    const path = String(input);
    requests.push(path);
    return new Response(JSON.stringify(path.includes('provider-registry') ? registry : { keys: ['fast'] }), { headers: { 'Content-Type': 'application/json' } });
  };
  let aliases: string[] | undefined;
  function Probe() { aliases = useModelProfileAliases('chat.fast.us'); return null; }
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => { renderer = create(createElement(Probe)); });
    assert.deepEqual(aliases, []);
    assert.equal(requests.some((path) => path.includes('map-keys')), false);
    registry = { models: {} };
    await act(async () => { window.dispatchEvent(new Event('zeroclaw-config-mutated')); });
    assert.deepEqual(aliases, ['fast']);
    registry = original;
    await act(async () => { window.dispatchEvent(new Event('zeroclaw-config-mutated')); });
    assert.deepEqual(aliases, []);
    assert.equal(requests.filter((path) => path.includes('map-keys')).length, 1);
  } finally { await act(async () => renderer?.unmount()); }
});

test('cost resource discovery includes canonical chat and voice models', async () => {
  const registry = structuredClone(original) as { connections: typeof original.connections; models: Record<string, unknown> };
  registry.models['voice.en.us'] = { capability: 'tts', provider: 'gateway.prod', model: 'tts-upstream' };
  registry.models['asr.en.us'] = { capability: 'asr', provider: 'gateway.prod', model: 'asr-upstream' };
  globalThis.fetch = async (input) => new Response(JSON.stringify(String(input).includes('provider-registry') ? registry : { templates: [] }), { headers: { 'Content-Type': 'application/json' } });
  assert.deepEqual(await configuredResourceIds('models', 'custom'), ['before']);
  assert.deepEqual(await configuredResourceIds('tts', 'custom'), ['tts-upstream']);
  assert.deepEqual(await configuredResourceIds('transcription', 'custom'), ['asr-upstream']);
});
