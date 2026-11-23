import assert from 'node:assert/strict';
import test from 'node:test';
import { Window } from 'happy-dom';

Object.defineProperty(globalThis, 'window', { value: new Window({ url: 'http://localhost' }) });

const {
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
