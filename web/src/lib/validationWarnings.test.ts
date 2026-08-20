import assert from 'node:assert/strict';
import test from 'node:test';

async function loadValidationWarningMessage() {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __ZEROCLAW_BASE__: '' },
  });
  const { validationWarningMessage } = await import('./validationWarnings.ts');
  delete (globalThis as { window?: unknown }).window;
  return validationWarningMessage;
}

test('unknown config warnings retain the API fallback message', async () => {
  const validationWarningMessage = await loadValidationWarningMessage();
  const message = validationWarningMessage({
    code: 'future_warning',
    message: 'future fallback',
    path: 'future.path',
  });

  assert.equal(message, 'future fallback');
});
