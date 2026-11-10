import assert from 'node:assert/strict';
import test from 'node:test';

import {
  resolveAvailableModels,
  type ModelProviderSources,
} from './modelPicker.logic.ts';

function daemon(values: string[]): ModelProviderSources {
  return {
    resolveAliasSource: async () => ({ values }),
  };
}

test('canonical resolver values are passed through unchanged', async () => {
  const refs = await resolveAvailableModels(
    daemon(['chat.fast.us', 'chat.backup.eu']),
  );
  assert.deepEqual(refs, ['chat.fast.us', 'chat.backup.eu']);
});

test('resolver errors remain visible to the caller', async () => {
  const sources: ModelProviderSources = {
    resolveAliasSource: async () => {
      throw new Error('resolver unavailable');
    },
  };
  await assert.rejects(resolveAvailableModels(sources), /resolver unavailable/);
});
