import assert from 'node:assert/strict';
import test from 'node:test';

import {
  resolveAvailableModels,
  scanConfiguredRefs,
  type ModelProviderSources,
} from './modelPicker.logic.ts';

/** A resolver-capable daemon: returns canonical sorted refs, listProps unused. */
function modernDaemon(values: string[]): ModelProviderSources {
  return {
    resolveAliasSource: async () => ({ values }),
    listProps: async () => {
      throw new Error('listProps must not be called when the resolver succeeds');
    },
  };
}

/** An older `web_dist_dir` daemon: resolver endpoint 404s, listProps works. */
function olderDaemon(paths: string[]): ModelProviderSources {
  return {
    resolveAliasSource: async () => {
      throw new Error('404: resolve-alias-source not found');
    },
    listProps: async () => ({ entries: paths.map((path) => ({ path })) }),
  };
}

// ── Resolver-success path ───────────────────────────────────────────────────

test('resolver success passes through the canonical sorted values', async () => {
  const refs = await resolveAvailableModels(
    modernDaemon(['anthropic.alpha', 'anthropic.omega', 'openai.zeta']),
  );
  assert.deepEqual(refs, ['anthropic.alpha', 'anthropic.omega', 'openai.zeta']);
});

// ── Older-daemon fallback path (the regression this guards) ──────────────────

test('resolver-unavailable falls back to a sorted listProps scan', async () => {
  // Deliberately unordered on the wire; the fallback must sort by family.alias.
  const refs = await resolveAvailableModels(
    olderDaemon([
      'providers.models.openai.zeta.model',
      'providers.models.anthropic.omega.model',
      'providers.models.anthropic.alpha.model',
      'providers.models.anthropic.alpha.api_key', // non-.model field ignored
    ]),
  );
  assert.deepEqual(refs, ['anthropic.alpha', 'anthropic.omega', 'openai.zeta']);
});

test('fallback dedupes multiple fields under one alias to a single ref', async () => {
  const refs = await resolveAvailableModels(
    olderDaemon([
      'providers.models.openai.zeta.model',
      'providers.models.openai.zeta.base_url',
    ]),
  );
  assert.deepEqual(refs, ['openai.zeta']);
});

// ── Both paths empty ────────────────────────────────────────────────────────

test('resolver-unavailable with no configured refs yields an empty list', async () => {
  const refs = await resolveAvailableModels(olderDaemon([]));
  assert.deepEqual(refs, []);
});

// ── scanConfiguredRefs unit behavior ────────────────────────────────────────

test('scanConfiguredRefs keeps only two-segment .model paths', () => {
  assert.deepEqual(
    scanConfiguredRefs([
      { path: 'providers.models.openai.zeta.model' },
      { path: 'providers.models.openai.model' }, // missing alias segment
      { path: 'providers.channels.discord.default.model' }, // wrong section
      { path: 'providers.models.anthropic.alpha.model' },
    ]),
    ['anthropic.alpha', 'openai.zeta'],
  );
});

test('scanConfiguredRefs tolerates an undefined entry list', () => {
  assert.deepEqual(scanConfiguredRefs(undefined), []);
});
