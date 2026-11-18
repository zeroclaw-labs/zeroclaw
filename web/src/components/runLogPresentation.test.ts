import assert from 'node:assert/strict';
import test from 'node:test';

import type { LogEvent } from '../lib/api';
import { presentRunLogEvent } from './runLogPresentation.ts';

function event(overrides: Partial<LogEvent>): LogEvent {
  return {
    id: 'event-1',
    '@timestamp': '2026-08-21T04:45:44.632Z',
    severity_number: 9,
    severity_text: 'INFO',
    event: { category: 'internal', action: 'note' },
    zeroclaw: {},
    attributes: {},
    ...overrides,
  };
}

test('model response surfaces provider, model, tokens, and requested tools', () => {
  const presentation = presentRunLogEvent(event({
    event: { category: 'provider', action: 'receive', outcome: 'success' },
    message: 'llm_response',
    zeroclaw: { model_provider: 'openai.mock' },
    attributes: {
      model: 'demo-model',
      iteration: 1,
      input_tokens: 10,
      output_tokens: 5,
      native_tool_calls: 1,
    },
  }));

  assert.equal(presentation.kind, 'model');
  assert.equal(presentation.title, 'Model response');
  assert.deepEqual(presentation.meta, [
    { label: 'Provider', value: 'openai.mock' },
    { label: 'Model', value: 'demo-model' },
    { label: 'Round', value: '1' },
    { label: 'Input', value: '10 tokens' },
    { label: 'Output', value: '5 tokens' },
    { label: 'Requested', value: '1 tool call' },
  ]);
});

test('unknown token counts stay absent instead of rendering as zero', () => {
  const presentation = presentRunLogEvent(event({
    event: { category: 'provider', action: 'receive', outcome: 'success' },
    message: 'llm_response',
    attributes: { model: 'demo-model', input_tokens: null, output_tokens: null },
  }));

  assert.deepEqual(presentation.meta, [{ label: 'Model', value: 'demo-model' }]);
});

test('tool result surfaces the tool, duration, and useful output', () => {
  const presentation = presentRunLogEvent(event({
    event: { category: 'tool', action: 'complete', outcome: 'success' },
    message: 'tool_call_result',
    zeroclaw: { duration_ms: 19 },
    attributes: {
      tool: 'shell',
      model: 'demo-model',
      iteration: 1,
      output: '2026-08-21T04:45:44Z\n',
    },
  }));

  assert.equal(presentation.kind, 'tool');
  assert.equal(presentation.title, 'shell completed');
  assert.equal(presentation.output, '2026-08-21T04:45:44Z');
  assert.deepEqual(presentation.meta, [
    { label: 'Tool', value: 'shell' },
    { label: 'Model', value: 'demo-model' },
    { label: 'Round', value: '1' },
    { label: 'Duration', value: '19 ms' },
  ]);
});

test('step completion highlights agent, captured tool count, and result', () => {
  const presentation = presentRunLogEvent(event({
    event: { category: 'internal', action: 'complete', outcome: 'success' },
    message: 'SOP audit: step 1 completed',
    attributes: {
      step: 1,
      status: 'completed',
      effective_agent: 'demo',
      tool_call_count: 1,
      output: 'The tool completed successfully.',
    },
  }));

  assert.equal(presentation.kind, 'step');
  assert.equal(presentation.eyebrow, 'Step 1');
  assert.equal(presentation.title, 'Step 1 completed');
  assert.equal(presentation.output, 'The tool completed successfully.');
  assert.deepEqual(presentation.meta, [
    { label: 'Agent', value: 'demo' },
    { label: 'Used', value: '1 tool call' },
  ]);
});
