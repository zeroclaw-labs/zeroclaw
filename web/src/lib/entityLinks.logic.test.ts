import assert from 'node:assert/strict';
import test from 'node:test';
import { entityConfigPath, modelCatalogTarget } from './entityLinks.ts';

test('catalog paths preserve dotted TOML profile and model keys', () => {
  const aliases = ['gw', 'gw.v2', 'gw.models.v2'];
  assert.deepEqual(modelCatalogTarget('providers.models.custom.gw.v2.model', aliases), { family: 'custom', alias: 'gw.v2', array: false });
  assert.deepEqual(modelCatalogTarget('providers.models.custom.gw.models.v2.models.fast.v1.id', aliases), { family: 'custom', alias: 'gw.models.v2', array: false });
  assert.deepEqual(modelCatalogTarget('providers.models.custom.gw.v2.fallback_models', aliases), { family: 'custom', alias: 'gw.v2', array: true });
  assert.equal(modelCatalogTarget('providers.models.custom.gw.v2.api_key', aliases), undefined);
});

test('provider links use the longest configured profile boundary', () => {
  const aliases = ['gw', 'gw.v2'];
  assert.equal(entityConfigPath('model-provider', 'custom.gw.fast', aliases), '/config/providers.models/custom/gw');
  assert.equal(entityConfigPath('model-provider', 'custom.gw.v2', aliases), '/config/providers.models/custom/gw.v2');
  assert.equal(entityConfigPath('model-provider', 'custom.gw.v2.fast', aliases), '/config/providers.models/custom/gw.v2');
  assert.equal(entityConfigPath('model-provider', 'openrouter.glm-5.2', ['glm-5.2']), '/config/providers.models/openrouter/glm-5.2');
});

test('unresolved provider links go to the provider list', () => {
  assert.equal(entityConfigPath('model-provider', 'custom.gw.fast'), '/config/providers.models');
  assert.equal(entityConfigPath('channel', 'telegram.work.v2'), '/config/channels/telegram/work.v2');
});
