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

test('known config warnings resolve through the dashboard catalog', async () => {
  const validationWarningMessage = await loadValidationWarningMessage();
  const auditMessage = validationWarningMessage({
    code: 'security_audit_enabled_has_no_effect',
    message: 'unlocalized audit fallback',
    path: 'security.audit.enabled',
  });

  assert.match(auditMessage, /audit logging.*not active/i);
  assert.doesNotMatch(auditMessage, /unlocalized audit fallback/);
});

test('unknown config warnings retain the API fallback message', async () => {
  const validationWarningMessage = await loadValidationWarningMessage();
  const message = validationWarningMessage({
    code: 'future_warning',
    message: 'future fallback',
    path: 'future.path',
  });

  assert.equal(message, 'future fallback');
});
