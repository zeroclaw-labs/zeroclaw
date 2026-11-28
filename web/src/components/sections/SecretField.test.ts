import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement, act } from 'react';
import { Window } from 'happy-dom';
import { createJiti } from 'jiti';

test('saved credentials show a fixed mask; replacement and cancel never read the saved key', async () => {
  const window = new Window();
  Object.assign(globalThis, { window, document: window.document, IS_REACT_ACT_ENVIRONMENT: true });
  const { createRoot } = await import('react-dom/client');
  const jiti = createJiti(import.meta.url, { jsx: { runtime: 'automatic' } });
  const { SecretField } = await jiti.import<typeof import('./SecretField.tsx')>('./SecretField.tsx');
  const container = document.createElement('div');
  document.body.append(container);
  const root = createRoot(container);
  const changes: string[] = [];
  const render = (value: string, populated = true) => act(async () => {
    root.render(createElement(SecretField, {
      inputId: 'providers.models.openrouter.primary.management_api_key',
      populated, value, onChange: (next: string) => changes.push(next),
    }));
  });
  try {
    await render('');
    assert.match(container.textContent ?? '', /\*{8}/);
    assert.equal(container.querySelector('input'), null);
    await act(async () => container.querySelector('button')!.click());
    assert.equal(container.querySelector('input')!.type, 'password');
    assert.equal(container.querySelector('input')!.value, '');
    await render('replacement-fixture');
    assert.equal(container.querySelector('input')!.type, 'password');
    const buttons = container.querySelectorAll('button');
    await act(async () => buttons[buttons.length - 1]!.click());
    assert.deepEqual(changes, ['']);
    await render('');
    assert.match(container.textContent ?? '', /\*{8}/);
    assert.equal(container.querySelector('input'), null);
    await render('', false);
    assert.equal(container.querySelector('input')!.type, 'password');
    assert.equal(container.querySelector('input')!.value, '');
  } finally {
    await act(async () => root.unmount());
    await window.happyDOM.close();
  }
});
