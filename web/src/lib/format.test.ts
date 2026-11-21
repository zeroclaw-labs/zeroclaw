import { test } from 'node:test';
import assert from 'node:assert/strict';

// format.ts pulls in the i18n module, whose imports read browser globals on
// load, so stand them in before importing it.
const storage = new Map<string, string>();
Object.assign(globalThis, {
  window: Object.assign(new EventTarget(), {
    location: { protocol: 'http:', host: 'localhost', pathname: '/', search: '' },
  }),
  localStorage: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => void storage.set(key, value),
    removeItem: (key: string) => void storage.delete(key),
  },
});
const { formatUsd } = await import('./format');

test('formatUsd shows no spend as $0.00 and keeps <$0.01 for spend under a cent', () => {
  assert.equal(formatUsd(null), '—');
  assert.equal(formatUsd(0), '$0.00');
  assert.equal(formatUsd(-0.5), '$0.00');
  assert.equal(formatUsd(0.004), '<$0.01');
  assert.equal(formatUsd(0.01), '$0.01');
  assert.equal(formatUsd(12.5), '$12.50');
});
