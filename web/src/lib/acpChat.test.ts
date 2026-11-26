import assert from 'node:assert/strict';
import test from 'node:test';
import { Window } from 'happy-dom';
import type { WsMessage } from '@/types/api';

Object.defineProperty(globalThis, 'window', { value: new Window({ url: 'http://localhost' }) });

const {
  AcpChatSocket,
  acpNotificationToWsMessage,
  acpPermissionToWsMessage,
  acpPromptResultToWsMessage,
  acpSessionUpdateToWsMessage,
} = await import('./acpChat.ts');

test('ACP content and tool updates feed the existing chat reducer', () => {
  assert.deepEqual(acpSessionUpdateToWsMessage({
    update: {
      sessionUpdate: 'agent_message_chunk',
      content: { type: 'text', text: 'hello' },
    },
  }), { type: 'chunk', content: 'hello' });
  assert.deepEqual(acpSessionUpdateToWsMessage({
    update: {
      sessionUpdate: 'agent_thought_chunk',
      content: { type: 'text', text: 'reasoning' },
    },
  }), { type: 'thinking', content: 'reasoning' });
  assert.deepEqual(acpSessionUpdateToWsMessage({
    update: {
      sessionUpdate: 'tool_call',
      toolCallId: 'call-1',
      name: 'shell',
      rawInput: { command: 'pwd' },
    },
  }), {
    type: 'tool_call', id: 'call-1', name: 'shell', args: { command: 'pwd' },
  });
  assert.deepEqual(acpSessionUpdateToWsMessage({
    update: {
      sessionUpdate: 'tool_call_update',
      toolCallId: 'call-1',
      name: 'shell',
      rawOutput: '/workspace',
    },
  }), {
    type: 'tool_result', id: 'call-1', name: 'shell', output: '/workspace',
  });
});

test('ACP custom notifications preserve trim and safeguard display fields', () => {
  assert.deepEqual(acpNotificationToWsMessage({
    jsonrpc: '2.0',
    method: '_zeroclaw/history_trimmed',
    params: {
      droppedMessages: 4,
      droppedTurns: 2,
      keptTurns: 3,
      reason: 'token_budget',
      tokenBudget: 8000,
      tokensBefore: 9000,
      tokensAfter: 7000,
      tokensAfterSource: 'calibrated',
      unsatisfiableFloor: false,
    },
  }), {
    type: 'history_trimmed',
    dropped_messages: 4,
    dropped_turns: 2,
    kept_turns: 3,
    reason: 'token_budget',
    token_budget: 8000,
    tokens_before: 9000,
    tokens_after: 7000,
    tokens_before_source: undefined,
    tokens_after_source: 'calibrated',
    unsatisfiable_floor: false,
  });
  assert.deepEqual(acpNotificationToWsMessage({
    jsonrpc: '2.0',
    method: '_zeroclaw/safeguard_fallback',
    params: {
      fallbackKind: 'client_server',
      requestedModel: 'requested',
      servedModel: 'served',
    },
  }), {
    type: 'safeguard_fallback',
    fallback_kind: 'client_server',
    requested_model: 'requested',
    served_model: 'served',
  });
});

test('ACP terminal metadata maps to context and usage without losing null usage', () => {
  assert.deepEqual(acpPromptResultToWsMessage({
    content: 'done',
    stopReason: 'end_turn',
    _meta: {
      zeroclaw: {
        inputTokens: 10,
        outputTokens: 4,
        maxContextTokens: 7000,
        modelContextWindow: 8000,
        lastInputTokens: null,
        contextInputTokens: 6500,
        contextInputTokensSource: 'calibrated',
      },
    },
  }), {
    type: 'done',
    content: 'done',
    full_response: 'done',
    input_tokens: 10,
    output_tokens: 4,
    max_context_tokens: 7000,
    model_context_window: 8000,
    context_input_tokens: 6500,
    context_input_tokens_source: 'calibrated',
    last_input_tokens: null,
  });
});

test('permission requests count down to the server deadline across replay', () => {
  const issued = 1_800_000_000_000;
  const request = {
    jsonrpc: '2.0' as const,
    id: 'permission-1',
    method: 'session/request_permission',
    params: { _meta: { zeroclaw: { expiresAtUnixMs: issued + 3_600_000 } } },
  };
  assert.equal(acpPermissionToWsMessage(request, issued).timeout_secs, 3600);
  assert.equal(acpPermissionToWsMessage(request, issued + 300_000).timeout_secs, 3300);
  assert.equal(acpPermissionToWsMessage(request, issued + 4_000_000).timeout_secs, 0);
});

test('ACP reconnect requests the turn cursor and keeps a single tool card', async (t) => {
  const sockets: TestSocket[] = [];
  class TestSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    readyState = TestSocket.CONNECTING;
    onopen?: () => void;
    onclose?: (event: { code: number }) => void;
    onmessage?: (event: { data: string }) => void;
    readonly sent: Array<{ id: string | number; method: string; params: unknown }> = [];

    constructor() { sockets.push(this); }
    open() {
      this.readyState = TestSocket.OPEN;
      this.onopen?.();
    }
    close() {
      this.readyState = 3;
      this.onclose?.({ code: 1006 });
    }
    send(json: string) { this.sent.push(JSON.parse(json)); }
    receive(frame: object) { this.onmessage?.({ data: JSON.stringify({ jsonrpc: '2.0', ...frame }) }); }
    reply(method: string, result: object) {
      const request = this.sent.find((frame) => frame.method === method);
      assert.ok(request, method);
      this.receive({ id: request.id, result });
      return request.params;
    }
  }
  const original = Object.getOwnPropertyDescriptor(globalThis, 'WebSocket');
  Object.defineProperty(globalThis, 'WebSocket', { configurable: true, value: TestSocket });
  t.after(() => {
    if (original) Object.defineProperty(globalThis, 'WebSocket', original);
    else Reflect.deleteProperty(globalThis, 'WebSocket');
  });

  const chat = new AcpChatSocket({ agentAlias: 'test', sessionId: 'conversation', autoReconnect: false });
  const updates: WsMessage[] = [];
  chat.onMessage = (message) => updates.push(message);
  t.after(() => chat.disconnect());

  async function connect(turnId: string) {
    chat.connect();
    const socket = sockets[sockets.length - 1];
    assert.ok(socket);
    socket.open();
    socket.reply('initialize', { protocolVersion: 1 });
    await Promise.resolve();
    socket.reply('session/resume', { messageCount: 1 });
    await Promise.resolve();
    const cursor = socket.reply('_zeroclaw/session/reattach', { inFlight: true, turnId });
    await Promise.resolve();
    return { socket, cursor };
  }

  const first = await connect('turn-one');
  const toolCall = {
    method: 'session/update',
    params: {
      sessionId: 'conversation',
      update: { sessionUpdate: 'tool_call', toolCallId: 'tool', title: 'test' },
      _meta: { zeroclawTurn: { sessionId: 'conversation', turnId: 'turn-one', seq: 3 } },
    },
  };
  first.socket.receive(toolCall);
  first.socket.close();

  const second = await connect('turn-one');
  assert.deepEqual(second.cursor, {
    sessionId: 'conversation', turnId: 'turn-one', afterSeq: 3, activeOnly: false, acceptPartial: true,
  });
  second.socket.receive(toolCall);
  second.socket.receive({
    ...toolCall,
    params: {
      ...toolCall.params,
      update: { ...toolCall.params.update, sessionUpdate: 'tool_call_update', body: 'done' },
      _meta: { zeroclawTurn: { sessionId: 'conversation', turnId: 'turn-one', seq: 4 } },
    },
  });
  assert.deepEqual(updates.filter((message) => message.type.startsWith('tool_')).map((message) => message.type),
    ['tool_call', 'tool_result']);

  assert.equal(updates.filter((message) => message.type === 'turn_reset').length, 0);
  second.socket.close();
  const third = await connect('turn-two');
  assert.equal(updates.filter((message) => message.type === 'turn_reset').length, 1);
  third.socket.receive({
    ...toolCall,
    params: {
      ...toolCall.params,
      _meta: { zeroclawTurn: { sessionId: 'conversation', turnId: 'turn-two', seq: 1 } },
    },
  });
  assert.equal(updates.filter((message) => message.type === 'tool_call').length, 2);

  for (const [seq, persisted] of [[2, true], [3, false]] as const) {
    third.socket.receive({
      method: '_zeroclaw/turn_complete',
      params: {
        sessionId: 'conversation',
        result: {
          content: 'answer',
          stopReason: 'end_turn',
          _meta: { zeroclaw: { transcriptPersisted: persisted } },
        },
        _meta: { zeroclawTurn: { sessionId: 'conversation', turnId: 'turn-two', seq } },
      },
    });
    assert.equal(updates[updates.length - 1]?.replayed_completion, persisted);
  }

  let closed = false;
  const close = chat.closeSession().then(() => { closed = true; });
  assert.equal(closed, false);
  assert.deepEqual(third.socket.reply('session/close', {}), { sessionId: 'conversation' });
  await close;
  assert.equal(closed, true);
  const closeAgain = chat.closeSession();
  const retry = third.socket.sent[third.socket.sent.length - 1];
  assert.equal(retry?.method, 'session/close');
  third.socket.receive({ id: retry.id, error: { code: -32000, message: 'Session not found' } });
  await closeAgain;

  await t.test('a refused resume closes without scheduling another socket', async (t) => {
    t.mock.timers.enable({ apis: ['setTimeout'] });
    const staleChat = new AcpChatSocket({ agentAlias: 'test', sessionId: 'deleted', reconnectDelay: 1 });
    t.after(() => staleChat.disconnect());
    const errors: WsMessage[] = [];
    staleChat.onMessage = (message) => errors.push(message);
    staleChat.connect();
    const socket = sockets[sockets.length - 1];
    assert.ok(socket);
    const count = sockets.length;
    socket.open();
    socket.reply('initialize', { protocolVersion: 1 });
    await Promise.resolve();
    const resume = socket.sent.find((frame) => frame.method === 'session/resume');
    assert.ok(resume);
    socket.receive({ id: resume.id, error: { code: -32012, message: 'Session unavailable' } });
    await Promise.resolve();
    t.mock.timers.tick(100);
    assert.equal(sockets.length, count);
    assert.equal(staleChat.connected, false);
    assert.equal(errors.filter((message) => message.type === 'error').length, 1);
  });
});
