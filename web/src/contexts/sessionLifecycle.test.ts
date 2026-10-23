import assert from 'node:assert/strict';
import test, { beforeEach } from 'node:test';
import React, { createElement } from 'react';
import {
  act,
  create,
  type ReactTestInstance,
  type ReactTestRenderer,
} from 'react-test-renderer';
import type { ApprovalDecision, SessionMessagesResponse, WsMessage } from '../types/api.ts';
import type {
  AgentContextValue,
  AgentSessionRuntime,
  SessionSocket,
} from './AgentContext.tsx';

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();

  get length(): number { return this.values.size; }
  clear(): void { this.values.clear(); }
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  key(index: number): string | null { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string): void { this.values.delete(key); }
  setItem(key: string, value: string): void { this.values.set(key, value); }
}

const storage = new MemoryStorage();
const fakeWindow = {
  location: { protocol: 'http:', host: 'localhost' },
  dispatchEvent: () => true,
};
const fakeDocument = {
  addEventListener: () => {},
  removeEventListener: () => {},
  createElement: () => ({
    style: {},
    focus: () => {},
    select: () => {},
    value: '',
  }),
  body: { appendChild: () => {}, removeChild: () => {} },
  execCommand: () => true,
};

Object.assign(globalThis, {
  React,
  localStorage: storage,
  window: fakeWindow,
  document: fakeDocument,
});
Object.defineProperty(globalThis, 'navigator', {
  configurable: true,
  value: { language: 'en-US', clipboard: { writeText: async () => {} } },
});
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

let listedSessions: Array<Record<string, unknown>> = [];
type SessionsResponder = () => Promise<Array<Record<string, unknown>>>;
let sessionsResponder: SessionsResponder | null = null;
interface ConfigPutRequest {
  path: string;
  value: unknown;
  comment?: string;
}
let configPutCalls: ConfigPutRequest[] = [];
// Session ids the mocked gateway reports as already gone / failing on DELETE.
let missingSessions = new Set<string>();
let deleteFailures = new Set<string>();
let configPutHandler: ((request: ConfigPutRequest) => Promise<Response>) | null = null;

// Every request URL, for tests that check what was (not) asked of the gateway.
const requestedUrls: string[] = [];

globalThis.fetch = async (input, init) => {
  const url = typeof input === 'string'
    ? input
    : input instanceof URL
      ? input.toString()
      : input.url;
  requestedUrls.push(`${init?.method ?? 'GET'} ${url.replace(/^.*?(\/api\/)/, '/api/')}`);
  let body: unknown;
  if (url.includes('/api/config/catalog')) body = { providers: [] };
  else if (url.includes('/api/status')) body = { model: 'test-model' };
  else if (url.includes('/api/config/prop') && init?.method === 'PUT') {
    const request = JSON.parse(String(init.body)) as ConfigPutRequest;
    configPutCalls.push(request);
    if (configPutHandler) return configPutHandler(request);
    body = { path: request.path, value: request.value };
  } else if (url.includes('/api/config/prop')) body = { path: '', value: '<unset>' };
  else if (url.includes('/api/config/list')) body = { entries: [] };
  else if (url.endsWith('/api/sessions')) {
    // `sessionsResponder` lets a test control when each listing resolves, so
    // out-of-order list responses can be reproduced deterministically.
    body = { sessions: sessionsResponder ? await sessionsResponder() : listedSessions };
  }
  else if (/\/api\/sessions\/[^/]+$/.test(url) && init?.method === 'DELETE') {
    // The gateway's real DELETE contract: a plain `{"error": ...}` body, not
    // the structured ConfigApiError envelope, on a missing row.
    const id = decodeURIComponent(url.slice(url.lastIndexOf('/') + 1));
    if (deleteFailures.has(id)) {
      return new Response('{"error":"backend failure"}', { status: 500 });
    }
    if (missingSessions.has(id)) {
      return new Response('{"error":"Session not found"}', { status: 404 });
    }
    body = { deleted: true, session_id: id };
  }
  else return new Response('{"error":"not found"}', { status: 404 });
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
};

const { AgentProvider, useAgent } = await import('./AgentContext.tsx');
const { deleteSession, HttpError, ApiError } = await import('../lib/api.ts');
const { DraftContext } = await import('../hooks/useDraft.ts');
const { AgentChatInner } = await import('../pages/AgentChat.tsx');
const { MemoryRouter } = await import('react-router-dom');
const { setOidcSession } = await import('../lib/auth.ts');
const { loadChatHistory, saveChatHistory } = await import('../lib/chatHistoryStorage.ts');

class Deferred<T> {
  readonly promise: Promise<T>;
  private resolvePromise!: (value: T) => void;
  private rejectPromise!: (reason: unknown) => void;

  constructor() {
    this.promise = new Promise<T>((resolve, reject) => {
      this.resolvePromise = resolve;
      this.rejectPromise = reject;
    });
  }

  resolve(value: T): void { this.resolvePromise(value); }
  reject(reason: unknown): void { this.rejectPromise(reason); }
}

class FakeSocket implements SessionSocket {
  onMessage: ((msg: WsMessage) => void) | null = null;
  onOpen: (() => void) | null = null;
  onClose: ((ev: CloseEvent) => void) | null = null;
  onError: ((ev: Event) => void) | null = null;
  connectCalls = 0;
  disconnectCalls = 0;
  sent: string[] = [];
  approvalResponses: Array<{ requestId: string; decision: ApprovalDecision }> = [];
  private open = false;

  constructor(
    readonly agentAlias: string,
    readonly sessionId: string,
    readonly autoReconnect: boolean | undefined = undefined,
  ) {}

  connect(): void { this.connectCalls += 1; }
  disconnect(): void { this.disconnectCalls += 1; this.open = false; }
  get connected(): boolean { return this.open; }
  sendMessage(content: string): void {
    if (!this.open) throw new Error('closed');
    this.sent.push(content);
  }
  sendApprovalResponse(requestId: string, decision: ApprovalDecision): void {
    this.approvalResponses.push({ requestId, decision });
  }
  emitOpen(): void { this.open = true; this.onOpen?.(); }
  emitClose(code = 1006): void {
    this.open = false;
    this.onClose?.({ code } as CloseEvent);
  }
  emitMessage(message: WsMessage): void { this.onMessage?.(message); }
}

type MessageFactory = () => Promise<SessionMessagesResponse>;
type DeleteFactory = () => Promise<{ deleted: boolean }>;

class FakeSessionRuntime implements AgentSessionRuntime {
  readonly sockets: FakeSocket[] = [];
  readonly deleteCalls: string[] = [];
  readonly renameCalls: Array<{ id: string; name: string }> = [];
  readonly messageCalls: string[] = [];
  readonly messagePlans = new Map<string, MessageFactory[]>();
  readonly deletePlans = new Map<string, DeleteFactory[]>();
  readonly renameDeferred = new Map<string, Deferred<{ session_id: string; name: string }>>();
  mintedIds: string[] = [];

  createSocket({ agentAlias, sessionId, autoReconnect }: {
    agentAlias: string;
    sessionId: string;
    autoReconnect?: boolean;
  }): FakeSocket {
    const socket = new FakeSocket(agentAlias, sessionId, autoReconnect);
    this.sockets.push(socket);
    return socket;
  }

  getMessages(sessionId: string): Promise<SessionMessagesResponse> {
    this.messageCalls.push(sessionId);
    const plan = this.messagePlans.get(sessionId)?.shift();
    return plan ? plan() : Promise.resolve(messagesResponse(sessionId, true));
  }

  delete(sessionId: string): Promise<{ deleted: boolean }> {
    this.deleteCalls.push(sessionId);
    const plan = this.deletePlans.get(sessionId)?.shift();
    return plan ? plan() : Promise.resolve({ deleted: true });
  }

  rename(sessionId: string, name: string): Promise<{ session_id: string; name: string }> {
    this.renameCalls.push({ id: sessionId, name });
    return this.renameDeferred.get(sessionId)?.promise
      ?? Promise.resolve({ session_id: sessionId, name });
  }

  mintId(): string {
    const id = this.mintedIds.shift();
    if (!id) throw new Error('No deterministic session id queued');
    return id;
  }

  queueMessages(sessionId: string, plan: MessageFactory): void {
    const plans = this.messagePlans.get(sessionId) ?? [];
    plans.push(plan);
    this.messagePlans.set(sessionId, plans);
  }

  queueDelete(sessionId: string, plan: DeleteFactory): void {
    const plans = this.deletePlans.get(sessionId) ?? [];
    plans.push(plan);
    this.deletePlans.set(sessionId, plans);
  }
}

function messagesResponse(
  sessionId: string,
  sessionPersistence: boolean,
  contents: string[] = [],
): SessionMessagesResponse {
  return {
    session_id: sessionId,
    session_persistence: sessionPersistence,
    messages: contents.map((content) => ({ role: 'user', content, created_at: null })),
  };
}

interface MountedChat {
  renderer: ReactTestRenderer;
  context(): AgentContextValue;
  drafts: Map<string, string>;
}

async function mountChat(
  runtime: FakeSessionRuntime,
  includeChat = false,
  reservedSessionIds?: readonly string[],
): Promise<MountedChat> {
  let currentContext: AgentContextValue | null = null;
  const drafts = new Map<string, string>();
  const draftStore = {
    getDraft: (key: string) => drafts.get(key) ?? '',
    setDraft: (key: string, value: string) => { drafts.set(key, value); },
    clearDraft: (key: string) => { drafts.delete(key); },
  };

  function Probe() {
    currentContext = useAgent();
    return null;
  }

  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(
      createElement(
        MemoryRouter,
        null,
        createElement(
          AgentProvider,
          {
            agentAlias: 'ops',
            sessionRuntime: runtime,
            reservedSessionIds,
            children: createElement(
              React.Fragment,
              null,
              createElement(Probe),
              includeChat
                ? createElement(
                  DraftContext.Provider,
                  { value: draftStore },
                  createElement(AgentChatInner, { agentAlias: 'ops' }),
                )
                : null,
            ),
          },
        ),
      ),
      {
        createNodeMock: (element) => element.type === 'textarea'
          ? { style: {}, focus: () => {}, scrollHeight: 24 }
          : { focus: () => {}, scrollIntoView: () => {}, contains: () => false },
      },
    );
    await Promise.resolve();
  });

  return {
    renderer,
    context: () => {
      if (!currentContext) throw new Error('Probe did not render');
      return currentContext;
    },
    drafts,
  };
}

async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  });
}

function nodeText(node: ReactTestInstance): string {
  return node.children
    .map((child) => typeof child === 'string' ? child : nodeText(child))
    .join('');
}

function textarea(renderer: ReactTestRenderer): ReactTestInstance {
  return renderer.root.findByType('textarea');
}

async function typeInComposer(renderer: ReactTestRenderer, value: string): Promise<void> {
  await act(async () => {
    textarea(renderer).props.onChange({
      target: { value, style: {}, scrollHeight: 24 },
    });
  });
}

async function openSocket(runtime: FakeSessionRuntime, index: number): Promise<void> {
  await act(async () => { runtime.sockets[index]!.emitOpen(); });
}

async function goToSession(mounted: MountedChat, sessionId: string): Promise<boolean> {
  let accepted = false;
  await act(async () => {
    accepted = mounted.context().goToSession(sessionId);
  });
  return accepted;
}

async function unmount(renderer: ReactTestRenderer): Promise<void> {
  await act(async () => { renderer.unmount(); });
}

beforeEach(() => {
  storage.clear();
  requestedUrls.length = 0;
  configPutCalls = [];
  missingSessions = new Set();
  deleteFailures = new Set();
  configPutHandler = null;
  sessionsResponder = null;
  storage.setItem('zeroclaw_active_session.ops', 'A');
  listedSessions = [
    {
      session_id: 'A', session_key: 'gw_A', name: 'First', message_count: 0,
      last_activity: '2026-08-05T00:00:00Z', created_at: '2026-08-05T00:00:00Z',
      agent_alias: 'ops', channel_id: null,
    },
    {
      session_id: 'B', session_key: 'gw_B', name: 'Second', message_count: 0,
      last_activity: '2026-08-04T00:00:00Z', created_at: '2026-08-04T00:00:00Z',
      agent_alias: 'ops', channel_id: null,
    },
  ];
});

for (const scenario of ['unknown', 'disabled'] as const) {
  test(`picker and /new fail closed when persistence is ${scenario}`, async () => {
    const runtime = new FakeSessionRuntime();
    runtime.queueMessages('A', scenario === 'unknown'
      ? () => Promise.reject(new Error('hydration failed'))
      : () => Promise.resolve(messagesResponse('A', false)));
    const mounted = await mountChat(runtime, true);
    await openSocket(runtime, 0);
    await settle();

    assert.equal(mounted.context().hydrated, true);
    assert.equal(mounted.context().sessionPersistence, scenario === 'unknown' ? null : false);

    const trigger = mounted.renderer.root.findAllByType('button')
      .find((button) => button.props.title === 'Conversations');
    assert.ok(trigger);
    await act(async () => { trigger.props.onClick(); });
    await settle();

    const buttons = mounted.renderer.root.findAllByType('button');
    assert.equal(buttons.some((button) => nodeText(button).includes('New conversation')), false);
    // Unknown storage must not be reported as confirmed-disabled storage.
    const notice = mounted.renderer.root.findAllByType('p').map(nodeText).join(' | ');
    assert.equal(notice.includes('does not store conversations'), scenario === 'disabled');
    assert.equal(notice.includes('session storage is confirmed'), scenario === 'unknown');
    const second = buttons.find((button) => nodeText(button).includes('Second'));
    assert.equal(second?.props.disabled, true);

    await typeInComposer(mounted.renderer, '/new');
    const send = mounted.renderer.root.findAllByType('button')
      .find((button) => button.props['aria-label'] === 'Send');
    assert.ok(send);
    await act(async () => { send.props.onClick(); });

    assert.equal(mounted.context().sessionId, 'A');
    assert.equal(storage.getItem('zeroclaw_active_session.ops'), 'A');
    assert.equal(runtime.sockets.length, 1);
    assert.ok(mounted.context().messages.some((message) =>
      message.content.includes('session storage is confirmed')));
    await unmount(mounted.renderer);
  });
}

test('selecting the active picker row closes the menu', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  const mounted = await mountChat(runtime, true);
  await openSocket(runtime, 0);
  await settle();

  const trigger = mounted.renderer.root.findAllByType('button')
    .find((button) => button.props.title === 'Conversations');
  assert.ok(trigger);
  await act(async () => { trigger.props.onClick(); });
  await settle();
  assert.equal(
    mounted.renderer.root.findAllByType('button')
      .find((button) => button.props.title === 'Conversations')?.props['aria-expanded'],
    true,
  );

  const activeRow = mounted.renderer.root.findAllByType('button')
    .find((button) => button.props.title !== 'Conversations' && nodeText(button).includes('First'));
  assert.ok(activeRow);
  await act(async () => { activeRow.props.onClick(); });

  assert.equal(mounted.context().sessionId, 'A');
  assert.equal(
    mounted.renderer.root.findAllByType('button')
      .find((button) => button.props.title === 'Conversations')?.props['aria-expanded'],
    false,
  );
  await unmount(mounted.renderer);
});

test('a conversation another pane owns can be neither opened nor deleted here', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  // A sibling pane of the same agent is live on conversation B.
  const mounted = await mountChat(runtime, true, ['B']);
  await openSocket(runtime, 0);
  await settle();

  assert.deepEqual([...mounted.context().reservedSessionIds], ['B']);

  const trigger = mounted.renderer.root.findAllByType('button')
    .find((button) => button.props.title === 'Conversations');
  assert.ok(trigger);
  await act(async () => { trigger.props.onClick(); });
  await settle();

  const buttons = () => mounted.renderer.root.findAllByType('button');
  const reservedRow = buttons()
    .find((button) => button.props.title === 'Open in another tab' && nodeText(button).includes('Second'));
  assert.ok(reservedRow, 'the sibling-owned row must render');
  assert.equal(reservedRow.props.disabled, true);

  // Clicking it anyway must not move this pane onto the sibling's conversation:
  // two sockets on one gateway session diverge and can abort each other.
  await act(async () => { reservedRow.props.onClick(); });
  await settle();
  assert.equal(mounted.context().sessionId, 'A');
  assert.equal(runtime.sockets.length, 1);

  const reservedDelete = buttons()
    .find((button) => button.props['aria-label'] === 'Delete conversation: Second');
  assert.ok(reservedDelete, 'the sibling-owned row must still offer a delete affordance');
  assert.equal(reservedDelete.props.disabled, true);
  await act(async () => { reservedDelete.props.onClick(); });
  await settle();
  assert.deepEqual(runtime.deleteCalls, []);

  // A conversation no sibling holds stays fully operable from this pane.
  const freeRow = buttons()
    .find((button) => button.props.title !== 'Conversations' && nodeText(button).includes('First'));
  assert.ok(freeRow);
  assert.equal(freeRow.props.disabled, false);
  const freeDelete = buttons()
    .find((button) => button.props['aria-label'] === 'Delete conversation: First');
  assert.equal(freeDelete?.props.disabled, false);

  await unmount(mounted.renderer);
});

test('switch resets capability, hydrates the target, and ignores the old socket', async () => {
  const runtime = new FakeSessionRuntime();
  const bHydration = new Deferred<SessionMessagesResponse>();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true, ['from A'])));
  runtime.queueMessages('B', () => bHydration.promise);
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  assert.equal(mounted.context().sessionId, 'B');
  assert.deepEqual(mounted.context().messages, []);
  assert.equal(mounted.context().hydrated, false);
  assert.equal(mounted.context().sessionPersistence, null);
  assert.equal(runtime.sockets[0]?.disconnectCalls, 1);
  assert.equal(runtime.sockets[1]?.sessionId, 'B');

  await openSocket(runtime, 1);
  await act(async () => {
    runtime.sockets[0]!.emitMessage({ type: 'message', content: 'late A' });
    runtime.sockets[0]!.emitOpen();
    bHydration.resolve(messagesResponse('B', true, ['from B']));
  });
  await settle();

  assert.equal(mounted.context().sessionId, 'B');
  assert.equal(mounted.context().sessionPersistence, true);
  assert.equal(mounted.context().hydrated, true);
  assert.deepEqual(mounted.context().messages.map((message) => message.content), ['from B']);
  await unmount(mounted.renderer);
});

test('a deferred model PUT rebuilds the latest selected session socket', async () => {
  const runtime = new FakeSessionRuntime();
  const configPut = new Deferred<Response>();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('B', () => Promise.resolve(messagesResponse('B', true)));
  configPutHandler = () => configPut.promise;
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  let pendingSwitch!: Promise<void>;
  await act(async () => {
    pendingSwitch = mounted.context().switchModel('kilo.new-model');
    await Promise.resolve();
  });
  assert.deepEqual(configPutCalls, [{
    path: 'agents.ops.model_provider',
    value: 'kilo.new-model',
  }]);
  assert.equal(mounted.context().modelLoading, true);

  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  assert.equal(runtime.sockets[1]?.sessionId, 'B');
  await openSocket(runtime, 1);
  await settle();
  // This socket was constructed before the config write committed, so merely
  // opening it must not claim the model switch succeeded.
  assert.equal(mounted.context().modelLoading, true);

  await act(async () => {
    configPut.resolve(new Response(JSON.stringify({
      path: 'agents.ops.model_provider',
      value: 'kilo.new-model',
    }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    }));
    await pendingSwitch;
  });
  await settle();

  assert.deepEqual(runtime.sockets.map((socket) => socket.sessionId), ['A', 'B', 'B']);
  assert.ok(runtime.sockets[1]!.disconnectCalls > 0);
  assert.equal(runtime.sockets[2]!.connectCalls, 1);
  assert.equal(mounted.context().sessionId, 'B');
  assert.equal(mounted.context().modelLoading, true);

  await openSocket(runtime, 2);
  assert.equal(mounted.context().modelLoading, false);
  await unmount(mounted.renderer);
});

test('manual rename is the only name write when the first message is sent', async () => {
  const runtime = new FakeSessionRuntime();
  const rename = new Deferred<{ session_id: string; name: string }>();
  runtime.renameDeferred.set('A', rename);
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  const pendingRename = mounted.context().renameConversation('A', 'Release planning');
  await act(async () => { mounted.context().sendMessage('Draft the rollout'); });
  assert.deepEqual(runtime.renameCalls, [{ id: 'A', name: 'Release planning' }]);
  assert.deepEqual(runtime.sockets[0]?.sent, ['Draft the rollout']);

  await act(async () => {
    rename.resolve({ session_id: 'A', name: 'Release planning' });
    await pendingRename;
  });
  assert.equal(runtime.renameCalls.length, 1);
  await unmount(mounted.renderer);
});

test('a listing that lands after an active delete cannot resurrect the deleted row', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.mintedIds.push('C');
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('C', () => Promise.resolve(messagesResponse('C', true)));
  const mounted = await mountChat(runtime, true);
  await openSocket(runtime, 0);
  await settle();

  const buttons = () => mounted.renderer.root.findAllByType('button');
  const trigger = buttons().find((button) => button.props.title === 'Conversations');
  assert.ok(trigger);
  await act(async () => { trigger.props.onClick(); });
  await settle();

  // Hold the listing open so it settles only after the delete has moved the
  // pane onto a new conversation. Both loads an active delete issues (the
  // trailing reload, and the one the sessionId change fires) observe this
  // post-delete server state, in which A no longer exists.
  const pending: Array<Deferred<Array<Record<string, unknown>>>> = [];
  sessionsResponder = () => {
    const deferred = new Deferred<Array<Record<string, unknown>>>();
    pending.push(deferred);
    return deferred.promise;
  };
  const afterDelete = listedSessions.filter((s) => s.session_id !== 'A');

  const deleteA = buttons().find((button) => button.props['aria-label'] === 'Delete conversation: First');
  assert.ok(deleteA);
  await act(async () => { deleteA.props.onClick(); });
  const confirm = buttons().find((button) => nodeText(button) === 'Delete');
  assert.ok(confirm);
  await act(async () => { void confirm.props.onClick(); });
  await settle();

  assert.equal(mounted.context().sessionId, 'C');
  assert.ok(pending.length >= 1, 'the delete must trigger a list refresh');

  await act(async () => {
    for (const deferred of pending) deferred.resolve(afterDelete);
    await Promise.resolve();
  });
  await settle();

  const rendered = buttons().map((button) => nodeText(button)).join('|');
  assert.equal(
    rendered.includes('First'),
    false,
    `the deleted conversation must not return under its old name: ${rendered}`,
  );
  assert.equal(
    rendered.includes('Conversation A'),
    false,
    `the deleted conversation must not be re-synthesized as the active row: ${rendered}`,
  );
  // The surviving conversation and the freshly minted active one are both shown.
  assert.ok(rendered.includes('Second'), `surviving conversation missing: ${rendered}`);
  assert.ok(rendered.includes('Conversation C'), `new active conversation missing: ${rendered}`);
  assert.equal(runtime.deleteCalls.filter((id) => id === 'A').length, 1);

  await unmount(mounted.renderer);
});

test('delete preserves inactive state and moves an active session exactly once', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.mintedIds.push('C');
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  const cHydration = new Deferred<SessionMessagesResponse>();
  runtime.queueMessages('C', () => cHydration.promise);
  storage.setItem('zeroclaw_chat_history_v1:B', '{"messages":[]}');
  storage.setItem('zeroclaw_chat_history_v1:A', '{"messages":[]}');
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  await act(async () => { await mounted.context().removeSession('B'); });
  assert.equal(mounted.context().sessionId, 'A');
  assert.equal(runtime.sockets.length, 1);
  assert.equal(storage.getItem('zeroclaw_chat_history_v1:B'), null);

  await act(async () => { await mounted.context().removeSession('A'); });
  await settle();
  assert.equal(mounted.context().sessionId, 'C');
  assert.equal(mounted.context().hydrated, false);
  assert.equal(mounted.context().sessionPersistence, null);
  assert.equal(storage.getItem('zeroclaw_chat_history_v1:A'), null);
  assert.equal(runtime.sockets[1]?.sessionId, 'C');
  await unmount(mounted.renderer);
});

test('the gateway\'s plain 404 on DELETE surfaces as a status-bearing HttpError', async () => {
  missingSessions.add('gone');
  await assert.rejects(deleteSession('gone'), (err: unknown) => (
    err instanceof HttpError
    && !(err instanceof ApiError)
    && err.status === 404
    && err.message === 'API 404: {"error":"Session not found"}'
  ));
  assert.deepEqual(await deleteSession('present'), { deleted: true, session_id: 'present' });
});

test('deleting an active row the gateway no longer has is treated as deleted', async () => {
  // The picker lists the active conversation even when GET /api/sessions
  // lacks it (another client deleted it, or no turn is stored yet). Deleting
  // that row goes through the real wrapper and hits the real 404 contract.
  const runtime = new FakeSessionRuntime();
  runtime.mintedIds.push('C');
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('C', () => Promise.resolve(messagesResponse('C', true)));
  runtime.queueDelete('A', () => deleteSession('A'));
  missingSessions.add('A');
  storage.setItem('zeroclaw_chat_history_v1:A', '{"messages":[]}');
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  await act(async () => { await mounted.context().removeSession('A'); });
  await settle();
  assert.deepEqual(runtime.deleteCalls, ['A']);
  assert.equal(mounted.context().sessionId, 'C');
  assert.equal(storage.getItem('zeroclaw_chat_history_v1:A'), null);
  assert.equal(runtime.sockets[1]?.sessionId, 'C');
  await unmount(mounted.renderer);
});

test('a non-404 delete failure keeps the active row and its cache', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.mintedIds.push('C');
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueDelete('A', () => deleteSession('A'));
  deleteFailures.add('A');
  storage.setItem('zeroclaw_chat_history_v1:A', '{"messages":[]}');
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  await act(async () => {
    await assert.rejects(mounted.context().removeSession('A'), (err: unknown) => (
      err instanceof HttpError && err.status === 500
    ));
  });
  assert.equal(mounted.context().sessionId, 'A');
  assert.equal(storage.getItem('zeroclaw_chat_history_v1:A'), '{"messages":[]}');
  assert.equal(runtime.sockets.length, 1);
  await unmount(mounted.renderer);
});

test('a listing fetched before an active delete cannot resurrect the deleted row', async () => {
  // The picker refresh started by reopening the menu is still in flight when
  // the operator deletes the active row. The post-delete refresh must not
  // coalesce onto that pre-delete GET and report its rows as the newest.
  const runtime = new FakeSessionRuntime();
  runtime.mintedIds.push('C');
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('C', () => Promise.resolve(messagesResponse('C', true)));
  runtime.queueDelete('A', () => deleteSession('A'));
  const preDelete = new Deferred<Array<Record<string, unknown>>>();
  let listingCalls = 0;
  sessionsResponder = () => {
    listingCalls += 1;
    // 1: the mount-time load, resolved at once so rows A/B are on screen.
    // 2: the refresh started by reopening the picker, held open across the
    //    delete (the pre-delete snapshot). Later: the gateway's post-delete
    //    state.
    if (listingCalls === 1) return Promise.resolve(listedSessions);
    if (listingCalls === 2) return preDelete.promise;
    return Promise.resolve(listedSessions.filter((s) => s.session_id !== 'A'));
  };
  const mounted = await mountChat(runtime, true);
  await openSocket(runtime, 0);
  await settle();
  // Reopening the picker starts the refresh that is still in flight during
  // the delete, while the rows from the mount-time load remain clickable.
  const trigger = mounted.renderer.root.findAllByType('button')
    .find((button) => button.props.title === 'Conversations');
  assert.ok(trigger);
  await act(async () => { trigger.props.onClick(); });
  await settle();

  const deleteButton = mounted.renderer.root.findAllByType('button')
    .find((button) => button.props['aria-label'] === 'Delete conversation: First');
  assert.ok(deleteButton, 'active row A ("First") should expose a delete action');
  await act(async () => { deleteButton.props.onClick(); });
  const confirm = mounted.renderer.root.findAllByType('button')
    .find((button) => nodeText(button) === 'Delete');
  assert.ok(confirm);
  await act(async () => { await confirm.props.onClick(); });
  await settle();
  assert.equal(mounted.context().sessionId, 'C');
  assert.deepEqual(runtime.deleteCalls, ['A']);

  // Now the pre-delete listing lands, carrying the deleted row. The menu is
  // still open from the delete, so no new refresh starts: whatever the rows
  // show here is what the operator sees.
  await act(async () => { preDelete.resolve(listedSessions); });
  await settle();
  const rows = mounted.renderer.root.findAllByType('button')
    .map(nodeText)
    .filter((text) => /^(First|Second|Conversation C)/.test(text));
  assert.equal(rows.some((text) => text.startsWith('First')), false,
    `deleted row resurrected: ${rows.join(' | ')}`);
  assert.ok(rows.some((text) => text.startsWith('Second')));
  assert.ok(rows.some((text) => text.startsWith('Conversation C')));
  assert.ok(listingCalls >= 3, `post-delete refresh must issue a fresh listing request (calls: ${listingCalls})`);
  await unmount(mounted.renderer);
});

test('a late active delete cannot replace a newer selected session', async () => {
  const runtime = new FakeSessionRuntime();
  const deleteA = new Deferred<{ deleted: boolean }>();
  runtime.queueDelete('A', () => deleteA.promise);
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('B', () => Promise.resolve(messagesResponse('B', true, ['B survives'])));
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  const pendingDelete = mounted.context().removeSession('A');
  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  await openSocket(runtime, 1);
  await settle();
  await act(async () => {
    deleteA.resolve({ deleted: true });
    await pendingDelete;
  });
  await settle();

  assert.equal(mounted.context().sessionId, 'B');
  assert.deepEqual(mounted.context().messages.map((message) => message.content), ['B survives']);
  assert.deepEqual(runtime.sockets.map((socket) => socket.sessionId), ['A', 'B']);
  await unmount(mounted.renderer);
});

test('a deferred inactive delete replaces the target if it becomes active', async () => {
  const runtime = new FakeSessionRuntime();
  const deleteB = new Deferred<{ deleted: boolean }>();
  const bHydration = new Deferred<SessionMessagesResponse>();
  const cHydration = new Deferred<SessionMessagesResponse>();
  runtime.mintedIds.push('C');
  runtime.queueDelete('B', () => deleteB.promise);
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('B', () => bHydration.promise);
  runtime.queueMessages('C', () => cHydration.promise);
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  const pendingDelete = mounted.context().removeSession('B');
  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  assert.equal(mounted.context().sessionId, 'B');
  assert.equal(mounted.context().sessionPersistence, null);

  await act(async () => {
    deleteB.resolve({ deleted: true });
    await pendingDelete;
  });
  await settle();

  assert.equal(mounted.context().sessionId, 'C');
  assert.equal(mounted.context().sessionPersistence, null);
  assert.equal(storage.getItem('zeroclaw_active_session.ops'), 'C');
  assert.deepEqual(runtime.sockets.map((socket) => socket.sessionId), ['A', 'B', 'C']);
  await unmount(mounted.renderer);
});

test('a deferred delete replaces its target after an A to B to A round trip', async () => {
  const runtime = new FakeSessionRuntime();
  const deleteA = new Deferred<{ deleted: boolean }>();
  const secondAHydration = new Deferred<SessionMessagesResponse>();
  const cHydration = new Deferred<SessionMessagesResponse>();
  runtime.mintedIds.push('C');
  runtime.queueDelete('A', () => deleteA.promise);
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('B', () => Promise.resolve(messagesResponse('B', true)));
  runtime.queueMessages('A', () => secondAHydration.promise);
  runtime.queueMessages('C', () => cHydration.promise);
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  const pendingDelete = mounted.context().removeSession('A');
  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  assert.equal(mounted.context().sessionPersistence, true);
  assert.equal(await goToSession(mounted, 'A'), true);
  await settle();
  assert.equal(mounted.context().sessionId, 'A');
  assert.equal(mounted.context().sessionPersistence, null);

  await act(async () => {
    deleteA.resolve({ deleted: true });
    await pendingDelete;
  });
  await settle();

  assert.equal(mounted.context().sessionId, 'C');
  assert.equal(mounted.context().sessionPersistence, null);
  assert.equal(storage.getItem('zeroclaw_active_session.ops'), 'C');
  assert.deepEqual(runtime.sockets.map((socket) => socket.sessionId), ['A', 'B', 'A', 'C']);
  await unmount(mounted.renderer);
});

test('composer drafts follow agent and session without crossing conversations', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('B', () => Promise.resolve(messagesResponse('B', true)));
  runtime.queueMessages('B', () => Promise.resolve(messagesResponse('B', true)));
  const mounted = await mountChat(runtime, true);
  await openSocket(runtime, 0);
  await settle();

  await typeInComposer(mounted.renderer, 'draft-A');
  assert.equal(mounted.drafts.get('agent-chat.ops.A'), 'draft-A');

  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  await openSocket(runtime, 1);
  await settle();
  assert.equal(textarea(mounted.renderer).props.value, '');
  await typeInComposer(mounted.renderer, 'draft-B');

  assert.equal(await goToSession(mounted, 'A'), true);
  await settle();
  await openSocket(runtime, 2);
  await settle();
  assert.equal(textarea(mounted.renderer).props.value, 'draft-A');

  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  await openSocket(runtime, 3);
  await settle();
  assert.equal(textarea(mounted.renderer).props.value, 'draft-B');
  await unmount(mounted.renderer);
});

test('a late close from the previous session socket cannot clear the active approval', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('B', () => Promise.resolve(messagesResponse('B', true)));
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  // A parks an approval, then the operator switches to B. The switch resets
  // A's transcript state, so A's approval is already gone before B is current.
  await act(async () => {
    runtime.sockets[0]!.emitMessage({
      type: 'approval_request', request_id: 'req-A', tool: 'shell', timeout_secs: 120,
    });
  });
  assert.equal(mounted.context().pendingApproval?.requestId, 'req-A');
  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  assert.equal(mounted.context().pendingApproval, null);
  assert.equal(runtime.sockets[0]?.disconnectCalls, 1);

  await openSocket(runtime, 1);
  await act(async () => {
    runtime.sockets[1]!.emitMessage({
      type: 'approval_request', request_id: 'req-B', tool: 'shell', timeout_secs: 120,
    });
  });
  assert.equal(mounted.context().pendingApproval?.requestId, 'req-B');

  // The browser delivers A's close asynchronously, after B is current and has
  // its own approval parked. A owns nothing live any more; B's banner stays.
  await act(async () => { runtime.sockets[0]!.emitClose(1000); });
  assert.equal(mounted.context().pendingApproval?.requestId, 'req-B');

  // B's own close still drops B's approval: the gateway auto-denies the parked
  // request when the socket that carried it goes away.
  await act(async () => { runtime.sockets[1]!.emitClose(1006); });
  assert.equal(mounted.context().pendingApproval, null);
  await unmount(mounted.renderer);
});

test('session switch disconnects both the effect-owned and replacement sockets', async () => {
  const runtime = new FakeSessionRuntime();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true)));
  runtime.queueMessages('B', () => Promise.resolve(messagesResponse('B', true)));
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();

  await act(async () => { mounted.context().clearAllMessages(); });
  await settle();
  assert.deepEqual(runtime.sockets.map((socket) => socket.sessionId), ['A', 'A']);

  assert.equal(await goToSession(mounted, 'B'), true);
  await settle();
  assert.ok(runtime.sockets[0]!.disconnectCalls > 0);
  assert.ok(runtime.sockets[1]!.disconnectCalls > 0);
  assert.equal(runtime.sockets[2]?.sessionId, 'B');

  await openSocket(runtime, 2);
  await act(async () => {
    runtime.sockets[1]!.emitOpen();
    runtime.sockets[1]!.emitMessage({ type: 'message', content: 'stale replacement' });
  });
  assert.equal(mounted.context().messages.some((message) =>
    message.content === 'stale replacement'), false);
  await unmount(mounted.renderer);
});

// A signed-in account without the operator dashboard (scoped dashboard chat).
const scopedSession = {
  kind: 'oidc' as const,
  provider: 'oidc.corp',
  principal_id: 'principal-a',
  display_name: 'Ada',
  admin: false,
  expires_at: 4_102_444_800,
  csrf_token: 'csrf-1',
  features: { chat: true, operator_pages: false },
};

function chatControls(renderer: ReactTestRenderer) {
  const buttons = renderer.root.findAllByType('button');
  return {
    attach: buttons.some((button) => button.props['aria-label'] === 'Attach image'),
    workspaceLink: renderer.root.findAll((node) =>
      typeof node.props.to === 'string' && node.props.to.endsWith('/workspace')).length > 0,
    modelReadOnly: renderer.root.findAll((node) =>
      node.type === 'span' && node.props['data-model-readonly'] !== undefined),
  };
}

async function openPicker(renderer: ReactTestRenderer): Promise<void> {
  const trigger = renderer.root.findAllByType('button')
    .find((button) => button.props.title === 'Conversations');
  assert.ok(trigger);
  await act(async () => { trigger.props.onClick(); });
  await settle();
}

test('a paired browser keeps model switching, upload and the workspace link', async () => {
  const runtime = new FakeSessionRuntime();
  const mounted = await mountChat(runtime, true);
  await openSocket(runtime, 0);
  await settle();
  const controls = chatControls(mounted.renderer);
  assert.equal(controls.attach, true);
  assert.equal(controls.workspaceLink, true);
  assert.equal(controls.modelReadOnly.length, 0);
  await unmount(mounted.renderer);
});

test('a scoped account chats without model switching, upload or the workspace link', async () => {
  setOidcSession(scopedSession);
  try {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountChat(runtime, true);
    await settle();

    const controls = chatControls(mounted.renderer);
    assert.equal(controls.attach, false);
    assert.equal(controls.workspaceLink, false);
    assert.equal(controls.modelReadOnly.length, 1);
    assert.ok(nodeText(controls.modelReadOnly[0]!).includes('test-model'));
    // Model info comes from the agent's status only, with no config reads.
    assert.deepEqual(mounted.context().availableModels, []);

    await typeInComposer(mounted.renderer, '/model other');
    const send = mounted.renderer.root.findAllByType('button')
      .find((button) => button.props['aria-label'] === 'Send');
    assert.ok(send);
    await act(async () => { send.props.onClick(); });
    assert.ok(mounted.context().messages.some((message) =>
      message.content === 'Not available for your account'));
    assert.equal(configPutCalls.length, 0);
    await unmount(mounted.renderer);
  } finally {
    setOidcSession(null);
  }
});

test('a paired browser still keeps terminal sessions out of the picker', async () => {
  listedSessions = [
    {
      session_id: 'R1', session_key: 'rpc_R1', name: 'Terminal chat', message_count: 2,
      last_activity: '2026-10-19T00:00:00Z', created_at: '2026-10-19T00:00:00Z',
      agent_alias: 'ops', channel_id: null,
    },
  ];
  const runtime = new FakeSessionRuntime();
  const mounted = await mountChat(runtime, true);
  await openSocket(runtime, 0);
  await settle();
  await openPicker(mounted.renderer);
  const labels = mounted.renderer.root.findAllByType('button').map(nodeText);
  assert.equal(labels.some((label) => label.includes('Terminal chat')), false);
  await unmount(mounted.renderer);
});

test('CAPABILITY_MISSING refuses the request but keeps the running turn', async () => {
  const runtime = new FakeSessionRuntime();
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();
  await act(async () => { mounted.context().sendMessage('hello'); });
  assert.equal(mounted.context().typing, true);

  await act(async () => {
    runtime.sockets[0]!.emitMessage({
      type: 'error', code: 'CAPABILITY_MISSING', message: 'Steering is not available here',
    } as WsMessage);
  });
  assert.equal(mounted.context().typing, true);
  assert.ok(mounted.context().messages.some((message) =>
    message.content.includes('Steering is not available here')));

  await act(async () => {
    runtime.sockets[0]!.emitMessage({
      type: 'error', code: 'AGENT_ERROR', message: 'The turn failed',
    } as WsMessage);
  });
  assert.equal(mounted.context().typing, false);
  await unmount(mounted.renderer);
});


function seedLocalTranscript(sessionId: string): void {
  saveChatHistory(sessionId, [
    { id: 'm1', role: 'user', content: 'an earlier question', timestamp: '2026-10-18T00:00:00Z', local: true },
    { id: 'm2', role: 'agent', content: 'an earlier answer', timestamp: '2026-10-18T00:00:01Z' },
  ]);
}

test('a paired browser falls back to its local transcript when the gateway has none', async () => {
  seedLocalTranscript('A');
  const runtime = new FakeSessionRuntime();
  runtime.queueMessages('A', () => Promise.reject(new Error('Session not found')));
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();
  assert.ok(mounted.context().messages.some((m) => m.content === 'an earlier answer'));
  await unmount(mounted.renderer);
});

test('a socket closed mid-turn stops the turn and reloads the transcript when it reopens', async () => {
  const runtime = new FakeSessionRuntime();
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();
  await act(async () => { mounted.context().sendMessage('hello'); });
  assert.equal(mounted.context().typing, true);

  // The session ends mid-turn (4401); the turn finishes on the server.
  runtime.queueMessages('A', () => Promise.resolve({
    session_id: 'A',
    session_persistence: true,
    messages: [
      { role: 'user', content: 'hello', created_at: null },
      { role: 'assistant', content: 'finished while away', created_at: null },
    ],
  }));
  const callsBefore = runtime.messageCalls.length;
  await act(async () => { runtime.sockets[0]!.emitClose(4401); });
  assert.equal(mounted.context().typing, false);
  // A session close code is the auth layer's to explain, not a config error.
  assert.equal(mounted.context().error, null);

  // The client reconnects (after re-auth) and the transcript catches up.
  await openSocket(runtime, 0);
  await settle();
  assert.equal(runtime.messageCalls.length, callsBefore + 1);
  assert.ok(mounted.context().messages.some((m) => m.content === 'finished while away'));
  await unmount(mounted.renderer);
});

test('a socket closed while idle reconnects without reloading the transcript', async () => {
  const runtime = new FakeSessionRuntime();
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();
  const callsBefore = runtime.messageCalls.length;
  await act(async () => { runtime.sockets[0]!.emitClose(1001); });
  await openSocket(runtime, 0);
  await settle();
  assert.equal(runtime.messageCalls.length, callsBefore);
  await unmount(mounted.renderer);
});

test('at the conversation limit the chat says so, without a config error, and Retry reconnects', async () => {
  const runtime = new FakeSessionRuntime();
  const mounted = await mountChat(runtime, true);
  await openSocket(runtime, 0);
  await settle();
  const socket = runtime.sockets[0]!;
  await act(async () => {
    socket.emitMessage({
      type: 'error', code: 'CONVERSATION_LIMIT', message: 'All your conversations are busy',
    } as WsMessage);
    socket.emitClose(4409);
  });
  assert.equal(mounted.context().conversationLimited, true);
  assert.equal(mounted.context().error, null);
  assert.ok(mounted.context().messages.some((m) => m.content.includes('All your conversations are busy')));
  const line = mounted.renderer.root.findAll((node) =>
    node.type === 'div' && node.props['data-conversation-limit'] !== undefined);
  assert.equal(line.length, 1);

  const connectsBefore = socket.connectCalls;
  const retry = line[0]!.findAllByType('button').find((b) => nodeText(b) === 'Retry');
  assert.ok(retry);
  await act(async () => { retry.props.onClick(); });
  assert.equal(socket.connectCalls, connectsBefore + 1);
  await openSocket(runtime, 0);
  assert.equal(mounted.context().conversationLimited, false);
  await unmount(mounted.renderer);
});

test('a conversation closed under the socket (4410) shows the frame, not a config error', async () => {
  const runtime = new FakeSessionRuntime();
  const mounted = await mountChat(runtime);
  await openSocket(runtime, 0);
  await settle();
  await act(async () => {
    runtime.sockets[0]!.emitMessage({
      type: 'error', code: 'CONVERSATION_CLOSED', message: 'This conversation was closed',
    } as WsMessage);
    runtime.sockets[0]!.emitClose(4410);
  });
  assert.equal(mounted.context().error, null);
  assert.equal(mounted.context().conversationLimited, false);
  assert.ok(mounted.context().messages.some((m) => m.content.includes('This conversation was closed')));
  await unmount(mounted.renderer);
});

function sessionStart(messageCount: number): WsMessage {
  return {
    type: 'session_start',
    session_id: 'A',
    resumed: messageCount > 0,
    message_count: messageCount,
  } as WsMessage;
}

const notFound = () => Promise.reject(new HttpError(404, '{"error":"Session not found"}'));

async function startSocket(runtime: FakeSessionRuntime, messageCount: number): Promise<void> {
  await openSocket(runtime, 0);
  await act(async () => { runtime.sockets[0]!.emitMessage(sessionStart(messageCount)); });
  await settle();
}

async function assertConversationControls(mounted: MountedChat, runtime: FakeSessionRuntime): Promise<void> {
  assert.equal(mounted.context().sessionPersistence, true);
  await act(async () => { await mounted.context().renameConversation('A', 'Renamed'); });
  assert.deepEqual(runtime.renameCalls, [{ id: 'A', name: 'Renamed' }]);
  runtime.mintedIds = ['C'];
  let started = false;
  await act(async () => { started = mounted.context().startNewSession(); });
  assert.equal(started, true);
  assert.equal(mounted.context().sessionId, 'C');
}

test('a transcript that failed to load loads again when the socket opens its conversation', async () => {
  {
    const runtime = new FakeSessionRuntime();
    runtime.queueMessages('A', notFound);
    const mounted = await mountChat(runtime);
    await settle();
    assert.equal(mounted.context().sessionPersistence, null);

    await startSocket(runtime, 0);
    assert.deepEqual(runtime.messageCalls, ['A', 'A']);
    await assertConversationControls(mounted, runtime);
    await unmount(mounted.renderer);
  }
});

test('a transcript request that failed while the socket opened its conversation is retried', async () => {
  {
    const runtime = new FakeSessionRuntime();
    const first = new Deferred<SessionMessagesResponse>();
    runtime.queueMessages('A', () => first.promise);
    const mounted = await mountChat(runtime);
    await startSocket(runtime, 0);
    // The request was sent before the conversation existed, and answers after.
    await act(async () => { first.reject(new HttpError(404, '{"error":"Session not found"}')); });
    await settle();
    assert.deepEqual(runtime.messageCalls, ['A', 'A']);
    await assertConversationControls(mounted, runtime);
    await unmount(mounted.renderer);
  }
});

const earlier = ['an earlier question', 'an earlier answer'];

async function mountWithHistory(runtime: FakeSessionRuntime): Promise<MountedChat> {
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', true, earlier)));
  const mounted = await mountChat(runtime);
  await startSocket(runtime, earlier.length);
  assert.ok(mounted.context().messages.some((m) => m.content === 'an earlier answer'));
  return mounted;
}

function assertStartedOver(mounted: MountedChat): void {
  const messages = mounted.context().messages;
  assert.equal(messages.some((m) => earlier.includes(m.content)), false);
  const notices = messages.filter((m) => m.notice);
  assert.equal(notices.length, 1);
  assert.ok(notices[0]!.content.includes('started over'));
  assert.equal(notices[0]!.ephemeral, true);
  assert.equal(mounted.context().error, null);
}

test('an idle reload that ended the conversation drops the old transcript on reconnect', async () => {
  {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountWithHistory(runtime);
    await act(async () => { runtime.sockets[0]!.emitClose(1001); });
    await startSocket(runtime, 0);
    assertStartedOver(mounted);
    await unmount(mounted.renderer);
  }
});

test('a reload in the middle of a turn that ended the conversation keeps only the notice', async () => {
  {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountWithHistory(runtime);
    await act(async () => { mounted.context().sendMessage('one more'); });
    const reload = new Deferred<SessionMessagesResponse>();
    runtime.queueMessages('A', () => reload.promise);
    await act(async () => { runtime.sockets[0]!.emitClose(1001); });
    // The reopen reloads the transcript; the conversation is new before that answers.
    await startSocket(runtime, 0);
    assertStartedOver(mounted);
    await act(async () => { reload.resolve(messagesResponse('A', true)); });
    await settle();
    assertStartedOver(mounted);
    assert.equal(mounted.context().messages.some((m) => m.content === 'one more'), false);
    assert.equal(mounted.context().sessionPersistence, true);
    await unmount(mounted.renderer);
  }
});

test('a reconnect that resumes the conversation keeps its transcript and loads nothing again', async () => {
  {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountWithHistory(runtime);
    await act(async () => { runtime.sockets[0]!.emitClose(1001); });
    await startSocket(runtime, earlier.length);
    assert.deepEqual(mounted.context().messages.map((m) => m.content), earlier);
    assert.deepEqual(runtime.messageCalls, ['A']);
    await unmount(mounted.renderer);
  }
});

test('a gateway that stores no conversations keeps the local transcript on reconnect', async () => {
  seedLocalTranscript('A');
  const runtime = new FakeSessionRuntime();
  runtime.queueMessages('A', () => Promise.resolve(messagesResponse('A', false)));
  const mounted = await mountChat(runtime);
  await startSocket(runtime, 0);
  await act(async () => { runtime.sockets[0]!.emitClose(1006); });
  await startSocket(runtime, 0);
  assert.ok(mounted.context().messages.some((m) => m.content === 'an earlier answer'));
  assert.equal(mounted.context().messages.some((m) => m.notice), false);
  assert.deepEqual(runtime.messageCalls, ['A']);
  await unmount(mounted.renderer);
});

// ---------------------------------------------------------------------------
// One conversation per chat socket (a signed-in account chatting through the
// core): no socket until a message, no history, nothing to resume.
// ---------------------------------------------------------------------------

const coreAdminSession = {
  ...scopedSession,
  admin: true,
  features: { chat: true, operator_pages: true, core_sessions: true },
};

async function withSession<T>(session: typeof scopedSession, run: () => Promise<T>): Promise<T> {
  setOidcSession(session);
  try {
    return await run();
  } finally {
    setOidcSession(null);
  }
}

function conversationStart(id: string): WsMessage {
  return { type: 'session_start', session_id: id, resumed: false, message_count: 0 } as WsMessage;
}

/** Send the first message, open its socket and let the conversation start. */
async function startConversation(
  mounted: MountedChat,
  runtime: FakeSessionRuntime,
  content: string,
  conversationId: string,
): Promise<FakeSocket> {
  const before = runtime.sockets.length;
  await act(async () => { mounted.context().sendMessage(content); });
  assert.equal(runtime.sockets.length, before + 1);
  const socket = runtime.sockets[before]!;
  await act(async () => { socket.emitOpen(); });
  await act(async () => { socket.emitMessage(conversationStart(conversationId)); });
  return socket;
}

async function answer(socket: FakeSocket, text: string): Promise<void> {
  await act(async () => {
    socket.emitMessage({ type: 'done', full_response: text } as WsMessage);
  });
}

const endedNotice = (mounted: MountedChat) =>
  mounted.context().messages.filter((m) => m.notice && m.content.includes('This conversation has ended'));

for (const [label, session] of [['a scoped account', scopedSession], ['an administrator on the core path', coreAdminSession]] as const) {
  test(`${label}: no socket, history or conversation list until the first message`, async () => {
    await withSession(session, async () => {
      const runtime = new FakeSessionRuntime();
      const mounted = await mountChat(runtime, true);
      await settle();
      assert.equal(runtime.sockets.length, 0);
      assert.deepEqual(runtime.messageCalls, []);
      assert.equal(requestedUrls.some((u) => u.includes('/api/sessions')), false);
      assert.equal(
        mounted.renderer.root.findAllByType('button').some((b) => b.props.title === 'Conversations'),
        false,
      );
      // The composer is usable: the first message starts the conversation.
      assert.equal(mounted.context().startsConversationOnSend, true);
      assert.equal(textarea(mounted.renderer).props.disabled, false);
      // Said up front: the conversation ends with the page.
      const note = mounted.renderer.root.findAll((node) =>
        node.type === 'p' && node.props['data-conversation-scope-note'] !== undefined);
      assert.equal(note.length, 1);
      assert.ok(nodeText(note[0]!).includes('This conversation ends when you leave or reload this page, or sign out.'));
      await unmount(mounted.renderer);
    });
  });
}

test('the first message opens one socket, which never reconnects, and goes out once the conversation exists', async () => {
  await withSession(scopedSession, async () => {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountChat(runtime);
    await act(async () => { mounted.context().sendMessage('hello'); });
    // Shown at once; nothing sent before the gateway says the conversation exists.
    assert.ok(mounted.context().messages.some((m) => m.content === 'hello' && m.local));
    assert.equal(runtime.sockets.length, 1);
    const socket = runtime.sockets[0]!;
    assert.equal(socket.autoReconnect, false);
    assert.equal(mounted.context().startsConversationOnSend, false);
    await act(async () => { socket.emitOpen(); });
    assert.deepEqual(socket.sent, []);
    await act(async () => { socket.emitMessage(conversationStart('core-1')); });
    assert.deepEqual(socket.sent, ['hello']);
    assert.equal(mounted.context().typing, true);

    // Stop aborts the conversation by the gateway's id, not the page's.
    await act(async () => { await mounted.context().abortSession(); });
    assert.ok(requestedUrls.includes('POST /api/sessions/core-1/abort'));
    assert.deepEqual(runtime.messageCalls, []);
    await unmount(mounted.renderer);
  });
});

for (const code of [1001, 4410, 4401, 4403, 1006]) {
  test(`a close (${code}) ends the conversation: the transcript stays with a notice, and nothing reconnects`, async () => {
    await withSession(scopedSession, async () => {
      const runtime = new FakeSessionRuntime();
      const mounted = await mountChat(runtime, true);
      const socket = await startConversation(mounted, runtime, 'hello', 'core-1');
      await answer(socket, 'hi there');
      if (code === 4410) {
        await act(async () => {
          socket.emitMessage({
            type: 'error', code: 'CONVERSATION_CLOSED', message: 'This conversation was closed',
          } as WsMessage);
        });
      }
      await act(async () => { socket.emitClose(code); });
      await settle();

      const contents = mounted.context().messages.map((m) => m.content);
      assert.ok(contents.includes('hello'));
      assert.ok(contents.includes('hi there'));
      assert.equal(endedNotice(mounted).length, 1);
      const lastMessage = mounted.context().messages[mounted.context().messages.length - 1];
      assert.equal(lastMessage?.content.includes('This conversation has ended'), true);
      assert.equal(mounted.context().error, null);
      assert.equal(mounted.context().startsConversationOnSend, true);
      assert.equal(runtime.sockets.length, 1);
      assert.equal(socket.connectCalls, 1);
      assert.equal(textarea(mounted.renderer).props.disabled, false);
      await unmount(mounted.renderer);
    });
  });
}

test('after a conversation ends, the next message starts a new one on a new socket', async () => {
  await withSession(scopedSession, async () => {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountChat(runtime);
    const first = await startConversation(mounted, runtime, 'hello', 'core-1');
    await answer(first, 'hi there');
    await act(async () => { first.emitClose(1001); });

    const second = await startConversation(mounted, runtime, 'again', 'core-2');
    assert.deepEqual(second.sent, ['again']);
    assert.deepEqual(first.sent, ['hello']);
    // The ended conversation stays above, marked, with the new one after it.
    const contents = mounted.context().messages.map((m) => m.content);
    const notice = contents.findIndex((c) => c.includes('This conversation has ended'));
    assert.ok(notice > contents.indexOf('hi there'));
    assert.ok(contents.indexOf('again') > notice);
    await act(async () => { await mounted.context().abortSession(); });
    assert.ok(requestedUrls.includes('POST /api/sessions/core-2/abort'));
    await unmount(mounted.renderer);
  });
});

test('a conversation refused at the limit keeps its message, and Retry sends it', async () => {
  await withSession(scopedSession, async () => {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountChat(runtime, true);
    await act(async () => { mounted.context().sendMessage('hello'); });
    const socket = runtime.sockets[0]!;
    await act(async () => {
      socket.emitOpen();
      socket.emitMessage({
        type: 'error',
        code: 'CONVERSATION_LIMIT',
        message: 'This sign-in already has 4 conversations open; close one to start another',
      } as WsMessage);
      socket.emitClose(4409);
    });
    assert.equal(mounted.context().conversationLimited, true);
    assert.deepEqual(socket.sent, []);
    assert.equal(endedNotice(mounted).length, 0);
    const line = mounted.renderer.root.findAll((node) =>
      node.type === 'div' && node.props['data-conversation-limit'] !== undefined);
    assert.equal(line.length, 1);
    assert.ok(nodeText(line[0]!).includes('All your conversations are open in other tabs or panes.'));

    const retry = line[0]!.findAllByType('button').find((b) => nodeText(b) === 'Retry');
    assert.ok(retry);
    await act(async () => { retry.props.onClick(); });
    assert.equal(socket.connectCalls, 2);
    await act(async () => { socket.emitOpen(); });
    await act(async () => { socket.emitMessage(conversationStart('core-1')); });
    assert.deepEqual(socket.sent, ['hello']);
    assert.equal(mounted.context().conversationLimited, false);
    await unmount(mounted.renderer);
  });
});

test('a new conversation closes this one, clears the page, and deletes nothing', async () => {
  await withSession(scopedSession, async () => {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountChat(runtime);
    const socket = await startConversation(mounted, runtime, 'hello', 'core-1');
    await answer(socket, 'hi there');
    let started = false;
    await act(async () => { started = mounted.context().startNewSession(); });
    await settle();
    assert.equal(started, true);
    assert.deepEqual(mounted.context().messages, []);
    assert.equal(socket.disconnectCalls > 0, true);
    assert.equal(runtime.sockets.length, 1);
    assert.equal(mounted.context().startsConversationOnSend, true);
    assert.deepEqual(runtime.deleteCalls, []);
    assert.equal(requestedUrls.some((u) => u.startsWith('DELETE')), false);
    // The next message starts the next conversation.
    await startConversation(mounted, runtime, 'fresh', 'core-2');
    assert.equal(runtime.sockets.length, 2);
    await unmount(mounted.renderer);
  });
});

test('a scoped chat never shows or keeps a local transcript copy', async () => {
  seedLocalTranscript('A');
  await withSession(scopedSession, async () => {
    const runtime = new FakeSessionRuntime();
    const mounted = await mountChat(runtime);
    await settle();
    assert.equal(mounted.context().messages.some((m) => m.content === 'an earlier answer'), false);
    storage.clear();
    const socket = await startConversation(mounted, runtime, 'hello', 'core-1');
    await answer(socket, 'hi there');
    await settle();
    assert.ok(mounted.context().messages.some((m) => m.content === 'hi there'));
    assert.deepEqual(loadChatHistory('A'), []);
    await unmount(mounted.renderer);
  });
});
