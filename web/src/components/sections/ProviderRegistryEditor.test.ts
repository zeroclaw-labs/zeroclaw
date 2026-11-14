import assert from 'node:assert/strict';
import test from 'node:test';
import React, { createElement, useState } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { MemoryRouter } from 'react-router-dom';

const location = { protocol: 'https:', host: 'zc.example', hostname: 'zc.example', port: '', pathname: '/config/providers.registry', search: '' };
Object.assign(globalThis, {
  React, location,
  window: Object.assign(new EventTarget(), { location }),
  IS_REACT_ACT_ENVIRONMENT: true,
});
const { default: Editor } = await import('./ProviderRegistryEditor.tsx');
const { ConfigDraftProvider, useConfigDraft } = await import('../../lib/draftStore.tsx');
const { configuredResourceIds } = await import('../../lib/configuredModels.ts');
const original = {
  connections: { 'gateway.prod': { kind: 'custom', auth: { mode: 'api_key', api_key: '********' } } },
  models: { 'chat.fast.us': { capability: 'chat', provider: 'gateway.prod', model: 'before' } },
  model_presets: {}, fallback: {},
};

function renderEditor(children: React.ReactElement) {
  return create(
    createElement(
      ConfigDraftProvider,
      null,
      createElement(MemoryRouter, null, children),
    ),
  );
}

test('registry editor saves exact keys and the original masked baseline atomically', async () => {
  let current = structuredClone(original);
  let saved: { expected: typeof original; registry: typeof original } | undefined;
  const requests: Array<{ expected: typeof original; registry: typeof original }> = [];
  let saves = 0;
  globalThis.fetch = async (_input, init) => {
    if (init?.method === 'PUT') {
      saved = JSON.parse(init.body as string);
      requests.push(saved!);
      current = structuredClone(saved!.registry);
      // The real endpoint always returns a masked snapshot after preserving a
      // newly submitted credential. A later CAS must use this projection.
      current.connections['gateway.prod'].auth.api_key = '********';
    }
    return new Response(JSON.stringify(init?.method === 'PUT' ? { saved: true } : current), { headers: { 'Content-Type': 'application/json' } });
  };
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => {
      renderer = create(
        createElement(
          ConfigDraftProvider,
          null,
          createElement(
            MemoryRouter,
            { initialEntries: ['/config/providers.registry?model=chat.fast.us'] },
            createElement(Editor, { onSaved: () => { saves++; } }),
          ),
        ),
      );
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

    // A second save must compare against the server's masked document, not the
    // plaintext candidate submitted by the first save.
    const modelTab = renderer!.root.findAllByType('button').find((button) => String(button.props.children).includes('Models'))!;
    await act(async () => modelTab.props.onClick());
    const modelTextarea = renderer!.root.findByType('textarea');
    await act(async () => modelTextarea.props.onChange({ target: { value: JSON.stringify({ 'chat.fast.us': { ...original.models['chat.fast.us'], model: 'after-again' } }) } }));
    await act(async () => { renderer!.root.findAllByType('button').find((button) => String(button.props.children).includes('Save'))!.props.onClick(); });
    assert.equal(requests.length, 2);
    const second = requests[1]!;
    assert.equal(second.expected.connections['gateway.prod'].auth.api_key, '********');
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
    await act(async () => {
      renderer = renderEditor(
        createElement(Editor, { onSaved: () => assert.fail('invalid JSON must not save') }),
      );
    });
    const textarea = renderer!.root.findByType('textarea');
    await act(async () => textarea.props.onChange({ target: { value: '{invalid' } }));
    const save = renderer!.root.findAllByType('button').find((button) => String(button.props.children).includes('Save'))!;
    await act(async () => { save.props.onClick(); });
    assert.equal(writes, 0);
    assert.equal(renderer!.root.findByProps({ role: 'alert' }).children.length > 0, true);
  } finally { await act(async () => renderer?.unmount()); }
});

test('registry drafts survive leaving and returning to the config section', async () => {
  let reads = 0;
  globalThis.fetch = async () => {
    reads++;
    return new Response(JSON.stringify(original), { headers: { 'Content-Type': 'application/json' } });
  };
  let showEditor!: React.Dispatch<React.SetStateAction<boolean>>;
  function Harness() {
    const [visible, setVisible] = useState(true);
    showEditor = setVisible;
    return visible ? createElement(Editor, { onSaved: () => undefined }) : null;
  }
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => {
      renderer = create(
        createElement(
          ConfigDraftProvider,
          null,
          createElement(MemoryRouter, null, createElement(Harness)),
        ),
      );
    });
    const textarea = renderer!.root.findByType('textarea');
    const edited = JSON.stringify({
      'chat.fast.us': { ...original.models['chat.fast.us'], model: 'after-navigation' },
    });
    await act(async () => textarea.props.onChange({ target: { value: edited } }));
    await act(async () => showEditor(false));
    await act(async () => showEditor(true));
    assert.equal(renderer!.root.findByType('textarea').props.value, edited);
    assert.equal(reads, 1);
  } finally { await act(async () => renderer?.unmount()); }
});

test('banner discard reloads the mounted editor from the server', async () => {
  globalThis.fetch = async () => new Response(JSON.stringify(original), { headers: { 'Content-Type': 'application/json' } });
  let discard!: () => void;
  function Harness() {
    discard = useConfigDraft().discardAll;
    return createElement(Editor, { onSaved: () => undefined });
  }
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => {
      renderer = create(createElement(ConfigDraftProvider, null, createElement(MemoryRouter, null, createElement(Harness))));
    });
    const textarea = renderer!.root.findByType('textarea');
    await act(async () => textarea.props.onChange({ target: { value: JSON.stringify({}) } }));
    assert.deepEqual(JSON.parse(renderer!.root.findByType('textarea').props.value), {});
    await act(async () => discard());
    assert.match(renderer!.root.findByType('textarea').props.value, /gateway\.prod/);
  } finally { await act(async () => renderer?.unmount()); }
});

test('banner save freezes registry editing through PUT and masked baseline refresh', async () => {
  let current = structuredClone(original);
  let finishWrite!: () => void;
  let finishRefresh!: () => void;
  let writes = 0;
  let reads = 0;
  globalThis.fetch = async (_input, init) => {
    if (init?.method === 'PUT') {
      writes++;
      current = JSON.parse(init.body as string).registry;
      await new Promise<void>((resolve) => { finishWrite = resolve; });
      return new Response(JSON.stringify({ saved: true }));
    }
    if (++reads > 1) await new Promise<void>((resolve) => { finishRefresh = resolve; });
    return new Response(JSON.stringify(current), { headers: { 'Content-Type': 'application/json' } });
  };
  let draft!: ReturnType<typeof useConfigDraft>;
  function Harness() {
    draft = useConfigDraft();
    return createElement(Editor, { onSaved: () => undefined });
  }
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => { renderer = renderEditor(createElement(Harness)); });
    await act(async () => renderer!.root.findAllByType('button').find((button) => String(button.props.children).includes('Models'))!.props.onClick());
    const edited = JSON.stringify({ 'chat.fast.us': { ...original.models['chat.fast.us'], model: 'saved' } });
    await act(async () => renderer!.root.findByType('textarea').props.onChange({ target: { value: edited } }));
    let pending!: ReturnType<typeof draft.saveAll>;
    await act(async () => { pending = draft.saveAll(); });
    assert.equal(renderer!.root.findByType('textarea').props.disabled, true);
    assert.equal(draft.providerRegistrySaveState, 'saving');
    await act(async () => renderer!.root.findByType('textarea').props.onChange({ target: { value: '{newer}' } }));
    assert.equal(renderer!.root.findByType('textarea').props.value, edited);
    let repeated!: ReturnType<typeof draft.saveAll>;
    await act(async () => { repeated = draft.saveAll(); });
    assert.equal(writes, 1);
    await act(async () => finishWrite());
    assert.equal(renderer!.root.findByType('textarea').props.disabled, true);
    await act(async () => { finishRefresh(); await Promise.all([pending, repeated]); });
    assert.equal(renderer!.root.findByType('textarea').props.disabled, false);
    assert.equal(draft.providerRegistryDraft?.dirty, false);
    assert.equal(draft.providerRegistryDraft?.expected.models['chat.fast.us']?.model, 'saved');
  } finally { await act(async () => renderer?.unmount()); }
});

test('a failed post-save refresh requires reload even after the editor remounts', async () => {
  let reads = 0;
  let writes = 0;
  globalThis.fetch = async (_input, init) => {
    if (init?.method === 'PUT') {
      writes++;
      return new Response(JSON.stringify({ saved: true }));
    }
    if (++reads === 2) return new Response('refresh failed', { status: 500 });
    return new Response(JSON.stringify(original), { headers: { 'Content-Type': 'application/json' } });
  };
  let draft!: ReturnType<typeof useConfigDraft>;
  let showEditor!: React.Dispatch<React.SetStateAction<boolean>>;
  function Harness() {
    draft = useConfigDraft();
    const [visible, setVisible] = useState(true);
    showEditor = setVisible;
    return visible ? createElement(Editor, { onSaved: () => undefined }) : null;
  }
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => { renderer = renderEditor(createElement(Harness)); });
    await act(async () => renderer!.root.findByType('textarea').props.onChange({ target: { value: '{}' } }));
    await act(async () => { await assert.rejects(draft.saveAll()); });
    assert.equal(draft.providerRegistrySaveState, 'refresh-required');
    await act(async () => showEditor(false));
    await act(async () => showEditor(true));
    assert.equal(renderer!.root.findByType('textarea').props.disabled, true);
    await act(async () => { await assert.rejects(draft.saveAll()); });
    assert.equal(writes, 1);
    await act(async () => renderer!.root.findAllByType('button').find((button) => String(button.props.children).includes('Reload'))!.props.onClick());
    assert.equal(renderer!.root.findByType('textarea').props.disabled, false);
    assert.equal(draft.providerRegistrySaveState, 'idle');
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
