import assert from 'node:assert/strict';
import test from 'node:test';

import { createElement, useEffect } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

import {
  loadChatHistory,
  saveChatHistory,
  uiMessagesToPersisted,
} from '../lib/chatHistoryStorage.ts';
import { contextExhaustedBubblePresentation } from './terminalExplanation.logic.ts';

const SESSION_ID = 'mounted-hydration-session';
const NOTICE = 'Turn stopped: context exhausted.';

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();

  get length(): number {
    return this.values.size;
  }

  clear(): void {
    this.values.clear();
  }

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  key(index: number): string | null {
    return [...this.values.keys()][index] ?? null;
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }
}

class FakeWebSocket {
  static readonly OPEN = 1;
  static readonly instances: FakeWebSocket[] = [];
  readonly readyState = FakeWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor() {
    FakeWebSocket.instances.push(this);
  }

  close(): void {}
  send(): void {}

  emit(message: unknown): void {
    this.onmessage?.({ data: JSON.stringify(message) });
  }
}

test('mounted provider keeps one persisted:false notice when server hydration is incomplete', async () => {
  FakeWebSocket.instances.length = 0;
  const storage = new MemoryStorage();
  Object.assign(globalThis, {
    IS_REACT_ACT_ENVIRONMENT: true,
    localStorage: storage,
    window: {
      location: { protocol: 'http:', host: 'localhost' },
      dispatchEvent: () => true,
    },
    WebSocket: FakeWebSocket,
  });

  storage.setItem('zeroclaw_session_id.default', SESSION_ID);
  saveChatHistory(
    SESSION_ID,
    uiMessagesToPersisted([
      {
        id: 'local-user',
        role: 'user',
        content: 'large request',
        local: true,
        timestamp: new Date('2026-08-24T00:00:00Z'),
      },
      {
        id: 'local-terminal-notice',
        role: 'agent',
        content: NOTICE,
        notice: true,
        timestamp: new Date('2026-08-24T00:00:01Z'),
        ...contextExhaustedBubblePresentation(false),
      },
    ]),
  );

  globalThis.fetch = async (input) => {
    const url = String(input);
    if (url.includes(`/api/sessions/${SESSION_ID}/messages`)) {
      return new Response(
        JSON.stringify({
          session_persistence: true,
          // The backend retained the user row but missed the terminal notice.
          messages: [
            {
              role: 'user',
              content: '[CURRENT DATE & TIME: 2026-08-24 00:00:00 UTC]\n\nlarge request',
              created_at: '2026-08-24T00:00:00Z',
            },
            { role: 'user', content: 'later request', created_at: '2026-08-24T00:00:02Z' },
            { role: 'assistant', content: 'later response', created_at: '2026-08-24T00:00:03Z' },
          ],
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    }
    if (url.includes('/api/config/catalog')) {
      return new Response(JSON.stringify({ providers: [] }), { status: 200 });
    }
    if (url.includes('/api/status')) {
      return new Response(JSON.stringify({ model: 'controlled', locale: 'en' }), { status: 200 });
    }
    if (url.includes('/api/config/prop')) {
      return new Response(JSON.stringify({ path: 'agent', value: '<unset>' }), { status: 200 });
    }
    if (url.includes('/api/config/resolve-alias-source')) {
      return new Response(JSON.stringify({ source: 'model_providers', values: [] }), {
        status: 200,
      });
    }
    return new Response('{}', { status: 200 });
  };

  const { AgentProvider, useAgent } = await import('./AgentContext.tsx');
  let observed: ReturnType<typeof useAgent>['messages'] = [];

  function Probe() {
    const context = useAgent();
    useEffect(() => {
      observed = context.messages;
    }, [context.messages]);
    return null;
  }

  let renderer: ReactTestRenderer | undefined;
  await act(async () => {
    renderer = create(
      createElement(
        AgentProvider,
        { agentAlias: 'default', children: createElement(Probe) },
      ),
    );
  });
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  assert.deepEqual(
    observed.map(({ role, content, notice }) => ({ role, content, notice })),
    [
      {
        role: 'user',
        content: '[CURRENT DATE & TIME: 2026-08-24 00:00:00 UTC]\n\nlarge request',
        notice: undefined,
      },
      { role: 'agent', content: NOTICE, notice: true },
      { role: 'user', content: 'later request', notice: undefined },
      { role: 'agent', content: 'later response', notice: undefined },
    ],
  );
  assert.equal(observed.filter((message) => message.content === NOTICE).length, 1);
  assert.equal(loadChatHistory(SESSION_ID).filter((message) => message.content === NOTICE).length, 1);

  await act(async () => renderer?.unmount());
});

test('mounted provider commits a failed terminal partial and restores it on reload', async () => {
  const storage = new MemoryStorage();
  const sessionId = 'terminal-partial-reload-session';
  FakeWebSocket.instances.length = 0;
  Object.assign(globalThis, {
    IS_REACT_ACT_ENVIRONMENT: true,
    localStorage: storage,
    window: {
      location: { protocol: 'http:', host: 'localhost' },
      dispatchEvent: () => true,
    },
    WebSocket: FakeWebSocket,
  });
  storage.setItem('zeroclaw_session_id.default', sessionId);

  let serverMessages: Array<{ role: string; content: string; created_at: string }> = [];
  globalThis.fetch = async (input) => {
    const url = String(input);
    if (url.includes(`/api/sessions/${sessionId}/messages`)) {
      return new Response(
        JSON.stringify({ session_persistence: true, messages: serverMessages }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      );
    }
    if (url.includes('/api/config/catalog')) {
      return new Response(JSON.stringify({ providers: [] }), { status: 200 });
    }
    if (url.includes('/api/status')) {
      return new Response(JSON.stringify({ model: 'controlled', locale: 'en' }), { status: 200 });
    }
    if (url.includes('/api/config/prop')) {
      return new Response(JSON.stringify({ path: 'agent', value: '<unset>' }), { status: 200 });
    }
    if (url.includes('/api/config/resolve-alias-source')) {
      return new Response(JSON.stringify({ source: 'model_providers', values: [] }), {
        status: 200,
      });
    }
    return new Response('{}', { status: 200 });
  };

  const { AgentProvider, useAgent } = await import('./AgentContext.tsx');
  let observed: ReturnType<typeof useAgent> | undefined;
  function Probe() {
    const context = useAgent();
    useEffect(() => {
      observed = context;
    }, [context]);
    return null;
  }
  const mount = () => create(
    createElement(
      AgentProvider,
      { agentAlias: 'default', children: createElement(Probe) },
    ),
  );

  let renderer: ReactTestRenderer | undefined;
  await act(async () => {
    renderer = mount();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  const socket = FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
  assert.ok(socket, 'provider must create the chat WebSocket');

  await act(async () => {
    socket.onopen?.();
    observed?.sendMessage('large request');
    socket.emit({ type: 'chunk', content: 'partial answer' });
    socket.emit({ type: 'context_exhausted', notice: NOTICE, persisted: false });
    socket.emit({ type: 'error', code: 'PROVIDER_ERROR', message: 'context overflow' });
  });

  const liveContext = observed as ReturnType<typeof useAgent> | undefined;
  assert.ok(liveContext, 'provider must expose the live chat context');
  assert.deepEqual(
    liveContext.messages.map(({ role, content, notice, terminalPartial }) => ({
      role,
      content,
      notice,
      terminalPartial,
    })),
    [
      { role: 'user', content: 'large request', notice: undefined, terminalPartial: undefined },
      { role: 'agent', content: 'partial answer', notice: undefined, terminalPartial: true },
      { role: 'agent', content: NOTICE, notice: true, terminalPartial: undefined },
    ],
  );
  assert.equal(liveContext.streamingContent, '');

  await act(async () => renderer?.unmount());
  serverMessages = [
    {
      role: 'user',
      content: '[CURRENT DATE & TIME: 2026-08-24 00:00:00 UTC]\n\nlarge request',
      created_at: '2026-08-24T00:00:00Z',
    },
  ];
  observed = undefined;
  await act(async () => {
    renderer = mount();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  const reloadedContext = observed as ReturnType<typeof useAgent> | undefined;
  assert.ok(reloadedContext, 'remounted provider must expose the hydrated context');
  assert.deepEqual(
    reloadedContext.messages.map(({ content, notice, terminalPartial }) => ({
      content,
      notice,
      terminalPartial,
    })),
    [
      {
        content: '[CURRENT DATE & TIME: 2026-08-24 00:00:00 UTC]\n\nlarge request',
        notice: undefined,
        terminalPartial: undefined,
      },
      { content: 'partial answer', notice: undefined, terminalPartial: true },
      { content: NOTICE, notice: true, terminalPartial: undefined },
    ],
  );

  await act(async () => renderer?.unmount());
});
