import assert from 'node:assert/strict';
import test from 'node:test';

import {
  APPROVAL_WILDCARD,
  applyAuthState,
  applyApprovalState,
  applyCustomPermission,
  applyStrictMode,
  approvalLevelCaveat,
  effectiveApprovalState,
  effectiveAuthState,
  filterPermissionCatalogEntries,
  isApprovalOnlyWildcard,
  isMcpAutoAdmitted,
  normalizeAllowedTools,
  normalizeAutonomyLevel,
  parseDenyAllToolsDraft,
  parseOptionalAllowedTools,
  profileLevelFromDraft,
  serializeAllowedTools,
  type ToolPermissionGridValue,
} from './ToolPermissionGrid.logic.ts';

function sets(value: ToolPermissionGridValue) {
  return {
    realAllowSet: new Set((value.allowedTools ?? []).filter((name) => name !== '__none__')),
    excludedSet: new Set(value.excludedTools),
    autoApproveSet: new Set(value.autoApprove),
    alwaysAskSet: new Set(value.alwaysAsk),
  };
}

test('permission catalog excludes discovered CLI executables', () => {
  const entries = [
    { name: 'shell', group: 'agent' as const },
    { name: 'git', group: 'cli' as const },
    { name: 'docker', group: 'cli' as const },
  ];

  assert.deepEqual(filterPermissionCatalogEntries(entries), [entries[0]]);
});

test('nonempty allowlists still auto-admit MCP names unless denied', () => {
  const value: ToolPermissionGridValue = {
    allowedTools: ['shell'],
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [],
    alwaysAsk: [],
  };
  const current = sets(value);

  assert.equal(
    effectiveAuthState({
      name: 'server__tool',
      strict: true,
      realAllowSet: current.realAllowSet,
      excludedSet: current.excludedSet,
    }),
    'allow',
  );
  assert.equal(
    isMcpAutoAdmitted({
      name: 'server__tool',
      strict: true,
      realAllowSet: current.realAllowSet,
      excludedSet: current.excludedSet,
    }),
    true,
  );
  assert.equal(
    effectiveAuthState({
      name: 'shell',
      strict: true,
      realAllowSet: current.realAllowSet,
      excludedSet: current.excludedSet,
    }),
    'allow',
  );

  const denied = sets({ ...value, excludedTools: ['server__tool'] });
  assert.equal(
    effectiveAuthState({
      name: 'server__tool',
      strict: true,
      realAllowSet: denied.realAllowSet,
      excludedSet: denied.excludedSet,
    }),
    'deny',
  );
});

test('deny-all flag does not auto-admit MCP names', () => {
  const value: ToolPermissionGridValue = {
    allowedTools: null,
    denyAllTools: true,
    excludedTools: [],
    autoApprove: [],
    alwaysAsk: [],
  };
  const current = sets(value);

  assert.equal(
    isMcpAutoAdmitted({
      name: 'server__tool',
      strict: true,
      realAllowSet: current.realAllowSet,
      excludedSet: current.excludedSet,
    }),
    false,
  );
  assert.equal(
    effectiveAuthState({
      name: 'server__tool',
      strict: true,
      realAllowSet: current.realAllowSet,
      excludedSet: current.excludedSet,
    }),
    'inherit',
  );
  assert.equal(
    effectiveAuthState({
      name: 'shell',
      strict: true,
      realAllowSet: current.realAllowSet,
      excludedSet: current.excludedSet,
    }),
    'inherit',
  );
});

test('legacy __none__ sentinel normalizes to the deny-all flag', () => {
  assert.deepEqual(normalizeAllowedTools(['__none__']), {
    allowedTools: null,
    denyAllTools: true,
  });
  assert.deepEqual(normalizeAllowedTools([]), { allowedTools: null, denyAllTools: false });
  assert.deepEqual(normalizeAllowedTools(['shell', '__none__']), {
    allowedTools: ['shell'],
    denyAllTools: false,
  });
});

test('optional allowed_tools draft round-trips three states', () => {
  const unrestricted = { allowedTools: null, denyAllTools: false };
  assert.deepEqual(parseOptionalAllowedTools(''), unrestricted);
  assert.deepEqual(parseOptionalAllowedTools('<unset>'), unrestricted);
  assert.deepEqual(parseOptionalAllowedTools('null'), unrestricted);
  // `[]` keeps its legacy unrestricted meaning in the config.
  assert.deepEqual(parseOptionalAllowedTools('[]'), unrestricted);
  // The legacy sentinel is the only array draft that means deny-all.
  assert.deepEqual(parseOptionalAllowedTools('["__none__"]'), {
    allowedTools: null,
    denyAllTools: true,
  });
  assert.deepEqual(parseOptionalAllowedTools('["shell"]'), {
    allowedTools: ['shell'],
    denyAllTools: false,
  });
  assert.equal(serializeAllowedTools(null), '');
  assert.equal(serializeAllowedTools(['shell']), '["shell"]');
  assert.equal(parseDenyAllToolsDraft('true'), true);
  assert.equal(parseDenyAllToolsDraft(' false '), false);
  assert.equal(parseDenyAllToolsDraft(''), false);
  assert.equal(parseDenyAllToolsDraft(undefined), false);
  assert.equal(parseDenyAllToolsDraft('<unset>'), false);
});

test('strict mode toggle writes the deny-all flag, not a sentinel or []', () => {
  const unrestricted: ToolPermissionGridValue = {
    allowedTools: null,
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [],
    alwaysAsk: [],
  };
  // Strict ON with nothing marked Allow is the explicit deny-all state.
  const denyAll = applyStrictMode(unrestricted, true);
  assert.equal(denyAll.allowedTools, null);
  assert.equal(denyAll.denyAllTools, true);
  // Strict OFF clears both gates.
  const off = applyStrictMode({ ...unrestricted, allowedTools: ['shell'] }, false);
  assert.equal(off.allowedTools, null);
  assert.equal(off.denyAllTools, false);
  // Allowing a tool writes an allowlist and exits deny-all.
  const allow = applyAuthState({ ...unrestricted, denyAllTools: true }, 'shell', 'allow', true);
  assert.deepEqual(allow.allowedTools, ['shell']);
  assert.equal(allow.denyAllTools, false);
  // Removing the last allowed tool under strict collapses to deny-all.
  const inherit = applyAuthState(
    { ...unrestricted, allowedTools: ['shell'] },
    'shell',
    'inherit',
    true,
  );
  assert.equal(inherit.allowedTools, null);
  assert.equal(inherit.denyAllTools, true);
});

test('approval wildcards follow runtime precedence', () => {
  const askWildcard = sets({
    allowedTools: null,
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [APPROVAL_WILDCARD, 'shell'],
    alwaysAsk: [APPROVAL_WILDCARD],
  });

  assert.equal(
    effectiveApprovalState({
      name: 'shell',
      autoApproveSet: askWildcard.autoApproveSet,
      alwaysAskSet: askWildcard.alwaysAskSet,
    }),
    'ask',
  );

  const autoWildcard = sets({
    allowedTools: null,
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [APPROVAL_WILDCARD],
    alwaysAsk: [],
  });
  assert.equal(
    effectiveApprovalState({
      name: 'shell',
      autoApproveSet: autoWildcard.autoApproveSet,
      alwaysAskSet: autoWildcard.alwaysAskSet,
    }),
    'auto',
  );

  const exactAskOverridesAutoWildcard = sets({
    allowedTools: null,
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [APPROVAL_WILDCARD],
    alwaysAsk: ['shell'],
  });
  assert.equal(
    effectiveApprovalState({
      name: 'shell',
      autoApproveSet: exactAskOverridesAutoWildcard.autoApproveSet,
      alwaysAskSet: exactAskOverridesAutoWildcard.alwaysAskSet,
    }),
    'ask',
  );
});

test('approval wildcard cannot enter authorization arrays through grid actions', () => {
  const base: ToolPermissionGridValue = {
    allowedTools: null,
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [],
    alwaysAsk: [],
  };

  assert.equal(isApprovalOnlyWildcard(APPROVAL_WILDCARD), true);
  assert.equal(isApprovalOnlyWildcard('shell'), false);
  assert.strictEqual(applyAuthState(base, APPROVAL_WILDCARD, 'deny', false), base);
  assert.strictEqual(applyAuthState(base, APPROVAL_WILDCARD, 'allow', true), base);
  assert.equal(applyCustomPermission(base, APPROVAL_WILDCARD, 'deny'), null);
  assert.equal(applyCustomPermission(base, ` ${APPROVAL_WILDCARD} `, 'allow'), null);
  assert.deepEqual(
    applyCustomPermission(base, APPROVAL_WILDCARD, 'ask')?.alwaysAsk,
    [APPROVAL_WILDCARD],
  );
  assert.deepEqual(
    applyCustomPermission(base, APPROVAL_WILDCARD, 'auto')?.autoApprove,
    [APPROVAL_WILDCARD],
  );
});

test('full/readonly surface a caveat but keep approval overrides live and clearable', () => {
  // full/readonly bypass approval PROMPTS, but the stored always_ask/auto_approve
  // entries are not inert - a non-empty always_ask still refuses independent
  // delegation with no level check - so the grid must show a caveat and keep the
  // control live, not lock it to an "effective" value.
  const withAlwaysAsk: ToolPermissionGridValue = {
    allowedTools: null,
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [],
    alwaysAsk: ['shell'],
  };
  const s = sets(withAlwaysAsk);

  // The caveat fires for full/readonly, and never for supervised.
  assert.equal(approvalLevelCaveat('full'), 'full');
  assert.equal(approvalLevelCaveat('readonly'), 'readonly');
  assert.equal(approvalLevelCaveat('supervised'), null);

  // The control reflects the STORED entry at every level (never collapsed to an
  // effective value that would hide what is actually configured).
  assert.equal(
    effectiveApprovalState({
      name: 'shell',
      autoApproveSet: s.autoApproveSet,
      alwaysAskSet: s.alwaysAskSet,
    }),
    'ask',
  );

  // A stored always_ask entry is clearable regardless of level, so an operator is
  // never trapped by an entry that is still load-bearing for delegation.
  assert.deepEqual(applyApprovalState(withAlwaysAsk, 'shell', 'inherit').alwaysAsk, []);
});

test('level normalization and FieldForm draft derivation', () => {
  assert.equal(normalizeAutonomyLevel('full'), 'full');
  assert.equal(normalizeAutonomyLevel('readonly'), 'readonly');
  assert.equal(normalizeAutonomyLevel('supervised'), 'supervised');
  // Unknown / empty / unset fall back to supervised so an unreadable level never
  // hides a real prompt state.
  assert.equal(normalizeAutonomyLevel(''), 'supervised');
  assert.equal(normalizeAutonomyLevel(undefined), 'supervised');
  assert.equal(normalizeAutonomyLevel('bogus'), 'supervised');

  // The grid reads the level from the parent's `.level` sibling leaf, exactly as
  // FieldForm supplies it.
  const draft = {
    'risk_profiles.dev.level': 'full',
    'risk_profiles.dev.always_ask': '["shell"]',
    'risk_profiles.ro.level': 'readonly',
  };
  assert.equal(profileLevelFromDraft(draft, 'risk_profiles.dev'), 'full');
  assert.equal(profileLevelFromDraft(draft, 'risk_profiles.ro'), 'readonly');
  // Group with no level leaf in the draft -> supervised default.
  assert.equal(profileLevelFromDraft(draft, 'risk_profiles.missing'), 'supervised');
});

test('custom names can be added to each permission state', () => {
  const base: ToolPermissionGridValue = {
    allowedTools: null,
    denyAllTools: false,
    excludedTools: [],
    autoApprove: [],
    alwaysAsk: [],
  };

  assert.deepEqual(applyCustomPermission(base, 'worker__dynamic', 'deny')?.excludedTools, [
    'worker__dynamic',
  ]);
  assert.deepEqual(applyCustomPermission(base, 'worker__dynamic', 'allow')?.allowedTools, [
    'worker__dynamic',
  ]);
  assert.deepEqual(applyCustomPermission(base, 'worker__dynamic', 'ask')?.alwaysAsk, [
    'worker__dynamic',
  ]);
  assert.deepEqual(applyCustomPermission(base, 'worker__dynamic', 'auto')?.autoApprove, [
    'worker__dynamic',
  ]);
});
