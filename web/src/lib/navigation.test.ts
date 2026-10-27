import assert from 'node:assert/strict';
import test from 'node:test';
import { activeWorkspace, featureSettingsPath, routeTitleKey } from './navigation';

test('Admin stays selected across operational dashboards and nested settings', () => {
  for (const path of ['/admin', '/system', '/sessions', '/agents', '/logs', '/doctor',
    '/tools', '/skills', '/integrations', '/cron', '/pairing', '/canvas', '/acp-console',
    '/config', '/config/agents/reviewer', '/config/providers.models/anthropic/dev', '/setup/gateway']) {
    assert.equal(activeWorkspace(path), '/admin', path);
  }
});

test('conversation, code, and SOP detail routes retain their workspaces', () => {
  assert.equal(activeWorkspace('/agent'), '/agent');
  assert.equal(activeWorkspace('/agent/reviewer/workspace'), '/agent');
  assert.equal(activeWorkspace('/code'), '/code');
  assert.equal(activeWorkspace('/sops/release/edit'), '/sops');
  assert.equal(activeWorkspace('/runs/release/run-1'), '/sops');
});

test('Home and lookalike path prefixes do not select a workspace', () => {
  for (const path of ['/', '/administrator', '/agents-old', '/coder', '/systematic']) {
    assert.equal(activeWorkspace(path), null, path);
  }
});

test('Admin has a page title and searches all configuration sections', () => {
  assert.equal(routeTitleKey('/admin'), 'workspace.admin');
  assert.equal(featureSettingsPath('/admin'), '/config');
  assert.equal(featureSettingsPath('/agent/reviewer'), '/config/agents/reviewer');
});
