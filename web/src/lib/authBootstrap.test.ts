import assert from 'node:assert/strict';
import test from 'node:test';

import {
  DEFAULT_VERIFY_PATH,
  postureFromHealth,
  signInResult,
  verifyPath,
  type PublicHealth,
} from './authBootstrap.ts';

// The preview gateway's healthy and degraded `/health` bodies.
const previewHealth: PublicHealth = {
  require_pairing: true,
  sign_in: { pairing_code: false, bearer: true, verify: '/api/gateway/core' },
};
const previewCoreDown: PublicHealth = {
  ...previewHealth,
  code: 'core_unavailable',
  error: 'The ZeroClaw core is not reachable: connection refused',
  hint: 'Start the core with `zeroclaw daemon`.',
};

test('a fresh browser on the preview must sign in with an existing token', () => {
  const posture = postureFromHealth(previewHealth, false);
  assert.equal(posture.authenticated, false);
  assert.equal(posture.requiresPairing, true);
  assert.equal(posture.loginMode, 'bearer');
  assert.equal(posture.banner, null);
});

test('a missing require_pairing never opens the dashboard', () => {
  for (const health of [{}, { status: 'ok' }, { require_pairing: undefined }, null]) {
    const posture = postureFromHealth(health as PublicHealth | null, false);
    assert.equal(posture.authenticated, false, JSON.stringify(health));
    assert.equal(posture.requiresPairing, true, JSON.stringify(health));
  }
});

test('only an explicit require_pairing false opens it, as the in-process gateway says', () => {
  const posture = postureFromHealth({ require_pairing: false, paired: false }, false);
  assert.equal(posture.authenticated, true);
  assert.equal(posture.requiresPairing, false);
  assert.equal(posture.loginMode, 'pairing');
});

test('the in-process gateway keeps its pairing-code dialog', () => {
  const posture = postureFromHealth({ require_pairing: true, paired: true }, false);
  assert.equal(posture.loginMode, 'pairing');
  assert.equal(posture.authenticated, false);
});

test('a degraded gateway shows its core-link banner and still asks for a token', () => {
  const posture = postureFromHealth(previewCoreDown, false);
  assert.equal(posture.authenticated, false);
  assert.equal(posture.loginMode, 'bearer');
  assert.deepEqual(posture.banner, {
    code: 'core_unavailable',
    error: 'The ZeroClaw core is not reachable: connection refused',
    hint: 'Start the core with `zeroclaw daemon`.',
  });
});

test('a stored token is used, and the sign-in mode is still known for when it fails', () => {
  const posture = postureFromHealth(previewHealth, true);
  assert.equal(posture.authenticated, true);
  assert.equal(posture.loginMode, 'bearer');
});

test('the journey: no token, a bad token, then a good one', () => {
  // A fresh browser lands on the sign-in screen.
  let posture = postureFromHealth(previewHealth, false);
  assert.equal(posture.authenticated, false);
  // A token the core refuses answers 401: still signed out, told why.
  const bad = signInResult(401, { error: 'credential rejected', code: 'auth_required' });
  assert.deepEqual(bad, { ok: false, error: 'That token was not accepted.', banner: null });
  posture = postureFromHealth(previewHealth, false);
  assert.equal(posture.authenticated, false, 'the sign-in screen is shown again');
  // A token the core accepts answers 200 with the core-link data.
  const good = signInResult(200, { principal_id: 'shared-operator' });
  assert.deepEqual(good, { ok: true });
  posture = postureFromHealth(previewHealth, true);
  assert.equal(posture.authenticated, true);
});

test('a core that is down during sign-in is a banner, not a rejected token', () => {
  const result = signInResult(503, {
    code: 'core_unavailable',
    error: 'the core is not reachable',
    hint: 'Start the core.',
  });
  assert.deepEqual(result, {
    ok: false,
    error: 'the core is not reachable',
    banner: { code: 'core_unavailable', error: 'the core is not reachable', hint: 'Start the core.' },
  });
});

test('the token is only ever checked against a same-origin path', () => {
  assert.equal(verifyPath(previewHealth), '/api/gateway/core');
  for (const verify of ['https://evil.example/x', '//evil.example/x', 'api/x', '/\\evil.example', '']) {
    assert.equal(
      verifyPath({ sign_in: { bearer: true, pairing_code: false, verify } }),
      DEFAULT_VERIFY_PATH,
      verify,
    );
  }
  assert.equal(verifyPath(null), DEFAULT_VERIFY_PATH);
});
