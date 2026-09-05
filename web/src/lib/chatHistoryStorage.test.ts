import assert from 'node:assert/strict';
import test from 'node:test';

import {
  mergeServerHistoryWithLocalNotices,
  type PersistedChatBubble,
} from './chatHistoryStorage.ts';

const NOTICE = 'Turn stopped: context exhausted.';

function bubble(
  id: string,
  role: 'user' | 'agent',
  content: string,
  notice = false,
): PersistedChatBubble {
  return {
    id,
    role,
    content,
    notice: notice || undefined,
    timestamp: `2026-08-24T00:00:0${id.length}.000Z`,
  };
}

test('server hydration retains one explicitly local terminal notice when the append was missing', () => {
  const server = [bubble('server-user', 'user', 'large request')];
  const local = [
    bubble('local-user', 'user', 'large request'),
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(
    merged.map(({ role, content, notice }) => ({ role, content, notice })),
    [
      { role: 'user', content: 'large request', notice: undefined },
      { role: 'agent', content: NOTICE, notice: true },
    ],
  );
});

test('server hydration does not duplicate a terminal notice that was committed', () => {
  const server = [
    bubble('server-user', 'user', 'large request'),
    bubble('server-notice', 'agent', NOTICE),
  ];
  const local = [
    bubble('local-user', 'user', 'large request'),
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.equal(merged.filter((message) => message.content === NOTICE).length, 1);
  assert.deepEqual(merged, server);
});

test('server hydration never resurrects ordinary local-only bubbles', () => {
  const server = [bubble('server-user', 'user', 'saved')];
  const local = [bubble('local-agent', 'agent', 'unsaved ordinary reply')];

  assert.deepEqual(mergeServerHistoryWithLocalNotices(server, local), server);
});

test('matching is count-aware across repeated terminal notices', () => {
  const server = [
    bubble('server-user-1', 'user', 'same request'),
    bubble('server-notice', 'agent', NOTICE),
    bubble('server-user-2', 'user', 'same request'),
  ];
  const local = [
    bubble('local-user-1', 'user', 'same request'),
    bubble('local-notice-1', 'agent', NOTICE, true),
    bubble('local-user-2', 'user', 'same request'),
    bubble('local-notice-2', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.equal(merged.filter((message) => message.content === NOTICE).length, 2);
  assert.equal(merged[merged.length - 1]?.id, 'local-notice-2');
});

test('retained notice stays before a later persisted turn', () => {
  const server = [
    bubble('server-user-1', 'user', 'large request'),
    bubble('server-user-2', 'user', 'later request'),
    bubble('server-agent-2', 'agent', 'later response'),
  ];
  const local = [
    bubble('local-user-1', 'user', 'large request'),
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(
    merged.map(({ content }) => content),
    ['large request', NOTICE, 'later request', 'later response'],
  );
});

test('runtime-enriched server anchor keeps the notice before a later turn', () => {
  const server = [
    bubble(
      'server-user-1',
      'user',
      '[CURRENT DATE & TIME: 2026-08-24 00:00:00 UTC]\n\nlarge request',
    ),
    bubble('server-user-2', 'user', 'later request'),
    bubble('server-agent-2', 'agent', 'later response'),
  ];
  const local = [
    { ...bubble('local-user-1', 'user', 'large request'), local: true },
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(
    merged.map(({ content }) => content),
    [
      '[CURRENT DATE & TIME: 2026-08-24 00:00:00 UTC]\n\nlarge request',
      NOTICE,
      'later request',
      'later response',
    ],
  );
});

test('anchorless retained notice deduplicates a copy already on the server', () => {
  const server = [bubble('server-notice', 'agent', NOTICE)];
  const local = [bubble('local-notice', 'agent', NOTICE, true)];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(merged, server);
  assert.equal(merged.filter((message) => message.content === NOTICE).length, 1);
});

test('failed persistence retains streamed partial and notice in canonical order', () => {
  const server = [bubble('server-user', 'user', 'large request')];
  const local = [
    { ...bubble('local-user', 'user', 'large request'), local: true },
    { ...bubble('local-partial', 'agent', 'partial answer'), terminalPartial: true },
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(
    merged.map(({ content }) => content),
    ['large request', 'partial answer', NOTICE],
  );
});

test('identical assistant text in a later turn cannot consume the retained notice', () => {
  const server = [
    bubble('server-user-1', 'user', 'large request'),
    bubble('server-user-2', 'user', 'quote the warning'),
    bubble('server-agent-2', 'agent', NOTICE),
  ];
  const local = [
    bubble('local-user-1', 'user', 'large request'),
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.equal(merged.filter((message) => message.content === NOTICE).length, 2);
  assert.equal(merged[1]?.id, 'local-notice');
  assert.equal(merged[3]?.id, 'server-agent-2');
});
