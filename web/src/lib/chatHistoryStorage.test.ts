import assert from 'node:assert/strict';
import test from 'node:test';
import {
  mapServerMessagesToPersisted,
  persistedToUiMessages,
  uiMessagesToPersisted,
} from './chatHistoryStorage';

test('typed media and literal marker text survive a chat history round trip', () => {
  const message = {
    id: 'user-media',
    role: 'user' as const,
    content: 'Literal [IMAGE:/example.png]',
    attachments: [
      { kind: 'image' as const, target: '/uploads/photo.png' },
      { kind: 'document' as const, target: '/uploads/report.pdf' },
    ],
    fileIds: ['file-1'],
    timestamp: new Date('2026-01-01T00:00:00.000Z'),
  };
  const persisted = JSON.parse(JSON.stringify(uiMessagesToPersisted([message])));
  assert.deepEqual(persistedToUiMessages(persisted), [{
    ...message,
    thinking: undefined,
    markdown: undefined,
    local: undefined,
    toolCall: undefined,
  }]);
});

test('server file identity remains available when media has no text', () => {
  const attachments = [{ kind: 'document' as const, target: '/uploads/report.pdf' }];
  const messages = mapServerMessagesToPersisted([
    { role: 'user', content: '', files: ['image-1'], attachments, created_at: '2026-01-01T00:00:00Z' },
    { role: 'assistant', content: '', files: ['image-2'], created_at: '2026-01-01T00:00:01Z' },
  ]);
  assert.deepEqual(messages.map((message) => message.fileIds), [['image-1'], ['image-2']]);
  assert.deepEqual(messages.map((message) => message.content), ['', '']);
  assert.deepEqual(messages[0]?.attachments, attachments);
});
