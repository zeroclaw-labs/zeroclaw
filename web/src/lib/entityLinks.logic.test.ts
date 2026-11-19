import assert from 'node:assert/strict';
import test from 'node:test';
import { entityConfigPath } from './entityLinks.ts';

test('canonical model links preserve the complete exact key', () => {
  assert.equal(entityConfigPath('model-provider', 'custom.gw.fast'), '/config/providers.registry?model=custom.gw.fast');
  assert.equal(entityConfigPath('model-provider', 'chat.fast.us'), '/config/providers.registry?model=chat.fast.us');
  assert.equal(entityConfigPath('model-provider', 'main'), '/config/providers.registry?model=main');
  assert.equal(entityConfigPath('model-provider', 'chat/a+b'), '/config/providers.registry?model=chat%2Fa%2Bb');
  assert.equal(entityConfigPath('channel', 'telegram.work.v2'), '/config/channels/telegram/work.v2');
});
