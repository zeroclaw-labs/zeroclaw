import assert from 'node:assert/strict';
import test from 'node:test';

import { configHref } from './integrations.logic.ts';

test('model providers route on the API config key, not a display-name slug', () => {
  assert.equal(
    configHref('Z.AI', 'AiModel', 'zai'),
    '/config/providers.models/zai',
  );
  assert.equal(
    configHref('Azure OpenAI', 'AiModel', 'azure'),
    '/config/providers.models/azure',
  );
});

test('chat channels route on the ChannelsConfig map key', () => {
  assert.equal(configHref('Telegram', 'Chat', 'telegram'), '/config/channels/telegram');
  assert.equal(configHref('WhatsApp', 'Chat', 'whatsapp'), '/config/channels/whatsapp');
  assert.equal(configHref('WhatsApp Web', 'Chat', 'whatsapp'), '/config/channels/whatsapp');
  assert.equal(configHref('NextCloud Talk', 'Chat', 'nextcloud_talk'), '/config/channels/nextcloud_talk');
  assert.equal(configHref('Gmail Push', 'Chat', 'gmail_push'), '/config/channels/gmail_push');
  assert.equal(configHref('WeCom WebSocket', 'Chat', 'wecom_ws'), '/config/channels/wecom_ws');
});

test('entries without a config key fall back to the bare section', () => {
  assert.equal(configHref('Custom Model', 'AiModel', null), '/config/providers.models');
  assert.equal(configHref('Mystery Chat', 'Chat', undefined), '/config/channels');
});

test('platform entries stay inert and tools keep their routes', () => {
  assert.equal(configHref('macOS', 'Platform', null), null);
  assert.equal(configHref('Cron', 'ToolsAutomation', null), '/cron');
  assert.equal(configHref('Browser', 'ToolsAutomation', null), '/config/browser');
  assert.equal(configHref('Unknown Tool', 'ToolsAutomation', null), '/tools');
  assert.equal(configHref('Mystery', 'SomethingElse', null), '/config');
});
