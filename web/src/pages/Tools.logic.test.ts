import assert from 'node:assert/strict';
import test from 'node:test';

import {
  applyToolAccessPatch,
  buildToolAccessPatch,
  type ToolAccess,
} from './Tools.logic.ts';

const denyAll: ToolAccess = {
  allowed: null,
  denyAll: true,
  excluded: [],
};

test('Allow from deny-all emits one atomic allowlist transition', async () => {
  const change = buildToolAccessPatch('default', 'shell', denyAll, true);
  assert.ok(change);

  const calls: unknown[] = [];
  await applyToolAccessPatch(change, async (ops) => calls.push(ops), () => {});

  assert.deepEqual(change.next, {
    allowed: ['shell'],
    denyAll: false,
    excluded: [],
  });
  assert.deepEqual(calls, [[
    {
      op: 'replace',
      path: 'risk_profiles.default.allowed_tools',
      value: ['shell'],
    },
    {
      op: 'replace',
      path: 'risk_profiles.default.deny_all_tools',
      value: false,
    },
  ]]);
});

test('rejected PATCH rolls optimistic tool access back', async () => {
  const change = buildToolAccessPatch('default', 'shell', denyAll, true);
  assert.ok(change);

  let state = denyAll;
  await assert.rejects(
    applyToolAccessPatch(
      change,
      async () => {
        throw new Error('config rejected');
      },
      (next) => {
        state = next;
      },
    ),
    /config rejected/,
  );
  assert.deepEqual(state, denyAll);
});
