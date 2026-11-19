import assert from 'node:assert/strict';
import test from 'node:test';
import React, { createElement } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { MemoryRouter } from 'react-router-dom';

const location = { protocol: 'https:', host: 'zc.example', hostname: 'zc.example', pathname: '/quickstart', search: '' };
Object.assign(globalThis, {
  React, location,
  window: Object.assign(new EventTarget(), { location }),
  IS_REACT_ACT_ENVIRONMENT: true,
});
const { default: Quickstart } = await import('./Quickstart.tsx');

test('Quickstart stages exact configured model keys and allows clearing and reselecting', async () => {
  globalThis.fetch = async () => new Response(JSON.stringify({
    quickstart_completed: false, agents: [], risk_profiles: [], runtime_profiles: [],
    default_runtime_profile: 'balanced', model_providers: ['chat.fast.us', 'main'],
    channels: [], unassigned_channels: [], storage: [], model_provider_types: [],
    channel_types: [], risk_presets: [], runtime_presets: [], memory_kinds: [], personality_files: [],
  }), { headers: { 'Content-Type': 'application/json' } });
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => { renderer = create(createElement(MemoryRouter, null, createElement(Quickstart))); });
    const picker = () => renderer!.root.findAllByType('select').find((select) =>
      select.findAllByType('option').some((option) => option.props.value === 'chat.fast.us'))!;
    assert.ok(picker());
    await act(async () => picker().props.onChange({ target: { value: 'chat.fast.us' } }));
    assert.match(JSON.stringify(renderer!.toJSON()), /chat\.fast\.us/);
    assert.equal(picker(), undefined, 'a staged selection replaces the form');
    const clear = renderer!.root.findAllByType('button').find((button) => button.props.title === 'Clear')!;
    assert.ok(clear);
    await act(async () => clear.props.onClick());
    assert.ok(picker(), 'clearing restores the exact-key picker');
    await act(async () => picker().props.onChange({ target: { value: 'main' } }));
    assert.match(JSON.stringify(renderer!.toJSON()), /main/);
    assert.equal(picker(), undefined, 'single-part keys are valid configured models');
  } finally { await act(async () => renderer?.unmount()); }
});

test('Quickstart requires canonical non-secret connection fields before staging a new model', async () => {
  globalThis.fetch = async (input) => {
    const request = String(input);
    const body = request.includes('/quickstart/fields') ? { fields: [
      { key: 'model', label: 'model', kind: 'string', required: true, is_secret: false },
      { key: 'extra.resource', label: 'extra.resource', kind: 'string', required: true, is_secret: false },
    ] } : request.includes('/catalog/models') ? { models: [], live: false } : {
      quickstart_completed: false, agents: [], risk_profiles: [], runtime_profiles: [],
      default_runtime_profile: 'balanced', model_providers: [], channels: [], unassigned_channels: [],
      storage: [], model_provider_types: [{ kind: 'azure', display_name: 'Azure', local: false }],
      channel_types: [], risk_presets: [], runtime_presets: [], memory_kinds: [], personality_files: [],
    };
    return new Response(JSON.stringify(body), { headers: { 'Content-Type': 'application/json' } });
  };
  let renderer: ReactTestRenderer | undefined;
  try {
    await act(async () => { renderer = create(createElement(MemoryRouter, null, createElement(Quickstart))); });
    const provider = renderer!.root.findAllByType('select').find((select) =>
      select.findAllByType('option').some((option) => option.props.value === 'azure'))!;
    await act(async () => provider.props.onChange({ target: { value: 'azure' } }));
    const model = renderer!.root.findAllByType('input').find((input) => input.props.list === 'qs-model-catalog')!;
    await act(async () => model.props.onChange({ target: { value: 'chat-deployment' } }));
    const add = () => renderer!.root.findAllByType('button').find((button) =>
      button.children.some((child) => child === 'Add'))!;
    assert.equal(add().props.disabled, true);
    const resource = renderer!.root.findAllByType('input').find((input) => input.props.required)!;
    assert.ok(resource);
    await act(async () => resource.props.onChange({ target: { value: 'example-resource' } }));
    assert.equal(add().props.disabled, false);
    await act(async () => add().props.onClick());
    assert.match(JSON.stringify(renderer!.toJSON()), /azure\.default/);
  } finally { await act(async () => renderer?.unmount()); }
});
