import assert from 'node:assert/strict';
import test from 'node:test';

import { settleToolCatalogResult } from './toolCatalog.logic.ts';
import type { CliTool, ToolSpec } from '../types/api.ts';

const agentTool: ToolSpec = {
  name: 'shell',
  description: 'Run a shell command',
  parameters: {},
};

const cliTool: CliTool = {
  name: 'git',
  path: '/usr/bin/git',
  version: '2.51.0',
  category: 'vcs',
};

test('catalog settling preserves CLI tools and warns when agent tools fail', () => {
  const result = settleToolCatalogResult(
    { status: 'rejected', reason: new Error('agent catalog unavailable') },
    { status: 'fulfilled', value: [cliTool] },
  );

  assert.deepEqual(result.entries.map((entry) => [entry.name, entry.group]), [['git', 'cli']]);
  assert.deepEqual(result.warnings, [
    { source: 'agent', message: 'agent catalog unavailable' },
  ]);
});

test('catalog settling preserves agent tools and warns when CLI tools fail', () => {
  const result = settleToolCatalogResult(
    { status: 'fulfilled', value: [agentTool] },
    { status: 'rejected', reason: 'CLI catalog unavailable' },
  );

  assert.deepEqual(result.entries.map((entry) => [entry.name, entry.group]), [
    ['shell', 'agent'],
  ]);
  assert.deepEqual(result.warnings, [{ source: 'cli', message: 'CLI catalog unavailable' }]);
});

test('catalog settling still fails when both sources fail', () => {
  assert.throws(
    () =>
      settleToolCatalogResult(
        { status: 'rejected', reason: new Error('agent catalog unavailable') },
        { status: 'rejected', reason: 'CLI catalog unavailable' },
      ),
    /agent catalog unavailable; CLI catalog unavailable/,
  );
});
