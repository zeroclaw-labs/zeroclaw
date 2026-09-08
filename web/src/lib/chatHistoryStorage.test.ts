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
  assert.deepEqual(
    merged.map(({ id, role, content }) => ({ id, role, content })),
    server.map(({ id, role, content }) => ({ id, role, content })),
  );
  // The committed copy stays the server's row; it only adopts the lifecycle
  // marker so a reload still recognises the notice for what it is.
  assert.equal(merged[1]?.notice, true);
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

  assert.deepEqual(
    merged.map(({ id, role, content }) => ({ id, role, content })),
    server.map(({ id, role, content }) => ({ id, role, content })),
  );
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

test('a repeated prompt evicted from the local window still anchors its own turn', () => {
  // localStorage keeps only the last MAX_MESSAGES bubbles, so the earlier
  // identical prompt is gone locally while both remain on the server. Counting
  // occurrences from the start would call the retained prompt the first one and
  // attach the terminal explanation to the older turn.
  const server = [
    bubble('server-user-1', 'user', 'same request'),
    bubble('server-agent-1', 'agent', 'older answer'),
    bubble('server-user-2', 'user', 'same request'),
    bubble('server-agent-2', 'agent', 'later answer'),
  ];
  const local = [
    { ...bubble('local-user-2', 'user', 'same request'), local: true },
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(
    merged.map(({ content }) => content),
    ['same request', 'older answer', 'same request', 'later answer', NOTICE],
  );
  assert.equal(merged.filter((message) => message.content === NOTICE).length, 1);
});

test('a lost failed-user append is restored ahead of its retained sequence', () => {
  // Gateway appends are per-message and continue after one fails, so the user
  // row can be the only casualty while the partial and notice are committed.
  const server = [
    bubble('server-partial', 'agent', 'partial answer'),
    bubble('server-notice', 'agent', NOTICE),
    bubble('server-user-later', 'user', 'later request'),
    bubble('server-agent-later', 'agent', 'later response'),
  ];
  const local = [
    { ...bubble('local-user', 'user', 'large request'), local: true },
    { ...bubble('local-partial', 'agent', 'partial answer'), terminalPartial: true },
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(
    merged.map(({ id, content }) => ({ id, content })),
    [
      { id: 'local-user', content: 'large request' },
      { id: 'server-partial', content: 'partial answer' },
      { id: 'server-notice', content: NOTICE },
      { id: 'server-user-later', content: 'later request' },
      { id: 'server-agent-later', content: 'later response' },
    ],
  );
  assert.equal(merged.filter((message) => message.content === NOTICE).length, 1);
});

test('the merged failed turn survives being fed back as the next local history', () => {
  // The provider mirrors every hydrated transcript into localStorage, so the
  // merge has to be idempotent on its own output: the second mount reads an
  // enriched anchor and server-owned terminal rows.
  const server = [
    bubble('server-partial', 'agent', 'partial answer'),
    bubble('server-notice', 'agent', NOTICE),
    bubble('server-user-later', 'user', 'later request'),
    bubble('server-agent-later', 'agent', 'later response'),
  ];
  const local = [
    { ...bubble('local-user', 'user', 'large request'), local: true },
    { ...bubble('local-partial', 'agent', 'partial answer'), terminalPartial: true },
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const first = mergeServerHistoryWithLocalNotices(server, local);
  const second = mergeServerHistoryWithLocalNotices(server, first);

  const expected = [
    { id: 'local-user', content: 'large request' },
    { id: 'server-partial', content: 'partial answer' },
    { id: 'server-notice', content: NOTICE },
    { id: 'server-user-later', content: 'later request' },
    { id: 'server-agent-later', content: 'later response' },
  ];
  assert.deepEqual(first.map(({ id, content }) => ({ id, content })), expected);
  assert.deepEqual(second.map(({ id, content }) => ({ id, content })), expected);
});

test('an enriched local anchor from a previous hydration still matches its server row', () => {
  // The mirrored copy of a hydrated server user row carries the runtime
  // envelope, so a raw comparison against the normalized server candidate would
  // fail on the second mount and push the notice to the end.
  const enriched = '[CURRENT DATE & TIME: 2026-08-24 00:00:00 UTC]\n\nlarge request';
  const server = [
    bubble('server-user-1', 'user', enriched),
    bubble('server-user-2', 'user', 'later request'),
    bubble('server-agent-2', 'agent', 'later response'),
  ];
  const local = [
    bubble('local-user-1', 'user', enriched),
    bubble('local-notice', 'agent', NOTICE, true),
  ];

  const merged = mergeServerHistoryWithLocalNotices(server, local);

  assert.deepEqual(
    merged.map(({ content }) => content),
    [enriched, NOTICE, 'later request', 'later response'],
  );
});
