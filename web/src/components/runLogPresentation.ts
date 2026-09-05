import type { LogEvent } from '../lib/api';

export type RunLogKind = 'model' | 'tool' | 'step' | 'run' | 'event';

export interface RunLogMeta {
  label: string;
  value: string;
}

export interface RunLogPresentation {
  kind: RunLogKind;
  eyebrow: string;
  title: string;
  meta: RunLogMeta[];
  output?: string;
}

function attribute(event: LogEvent, key: string): unknown {
  return event.attributes?.[key];
}

function stringValue(value: unknown): string | undefined {
  if (typeof value === 'string' && value.trim()) return value.trim();
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return undefined;
}

function eventValue(event: LogEvent, key: string): string | undefined {
  return stringValue(attribute(event, key)) ?? stringValue(event.zeroclaw?.[key]);
}

function numberValue(value: unknown): number | undefined {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value === 'string' && value.trim()) {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) return parsed;
  }
  return undefined;
}

function pushMeta(meta: RunLogMeta[], label: string, value?: string): void {
  if (value && !meta.some((item) => item.label === label && item.value === value)) {
    meta.push({ label, value });
  }
}

function plural(value: number, singular: string): string {
  return `${value} ${singular}${value === 1 ? '' : 's'}`;
}

function outputPreview(event: LogEvent): string | undefined {
  const output =
    stringValue(attribute(event, 'output')) ?? stringValue(attribute(event, 'raw_response'));
  if (!output || output === '(no output)') return undefined;
  return output.length > 220 ? `${output.slice(0, 217)}…` : output;
}

export function presentRunLogEvent(event: LogEvent): RunLogPresentation {
  const message = event.message?.trim() || `${event.event.category}.${event.event.action}`;
  const meta: RunLogMeta[] = [];
  const model = eventValue(event, 'model');
  const provider = eventValue(event, 'model_provider');
  const tool = eventValue(event, 'tool');
  const iteration = stringValue(attribute(event, 'iteration'));

  if (event.event.category === 'provider') {
    pushMeta(meta, 'Provider', provider);
    pushMeta(meta, 'Model', model);
    pushMeta(meta, 'Round', iteration);
    const inputTokens = numberValue(attribute(event, 'input_tokens'));
    const outputTokens = numberValue(attribute(event, 'output_tokens'));
    if (inputTokens !== undefined) pushMeta(meta, 'Input', plural(inputTokens, 'token'));
    if (outputTokens !== undefined) pushMeta(meta, 'Output', plural(outputTokens, 'token'));
    const toolCalls = numberValue(attribute(event, 'native_tool_calls'));
    if (toolCalls !== undefined && toolCalls > 0) {
      pushMeta(meta, 'Requested', plural(toolCalls, 'tool call'));
    }
    return {
      kind: 'model',
      eyebrow: 'Model',
      title: event.event.action === 'send' ? 'Model request' : 'Model response',
      meta,
      output: event.event.action === 'receive' ? outputPreview(event) : undefined,
    };
  }

  if (event.event.category === 'tool') {
    pushMeta(meta, 'Tool', tool);
    pushMeta(meta, 'Model', model);
    pushMeta(meta, 'Round', iteration);
    if (event.zeroclaw?.duration_ms !== undefined) {
      pushMeta(meta, 'Duration', `${event.zeroclaw.duration_ms} ms`);
    }
    const succeeded = event.event.outcome === 'success';
    const failed = event.event.outcome === 'failure';
    return {
      kind: 'tool',
      eyebrow: 'Tool',
      title: failed
        ? `${tool || 'Tool'} failed`
        : succeeded
          ? `${tool || 'Tool'} completed`
          : `${tool || 'Tool'} started`,
      meta,
      output: outputPreview(event),
    };
  }

  const step = stringValue(attribute(event, 'step'));
  if (step) {
    const status = eventValue(event, 'status') ?? event.event.outcome;
    pushMeta(meta, 'Agent', eventValue(event, 'effective_agent'));
    const toolCallCount = numberValue(attribute(event, 'tool_call_count'));
    if (toolCallCount !== undefined) {
      pushMeta(meta, 'Used', plural(toolCallCount, 'tool call'));
    }
    return {
      kind: 'step',
      eyebrow: `Step ${step}`,
      title: status ? `Step ${step} ${status.replace(/_/g, ' ')}` : message,
      meta,
      output: outputPreview(event),
    };
  }

  if (eventValue(event, 'run_id') || event.zeroclaw?.sop_run_id) {
    pushMeta(meta, 'SOP', eventValue(event, 'sop_name'));
    return {
      kind: 'run',
      eyebrow: 'Run',
      title: message,
      meta,
    };
  }

  pushMeta(meta, 'Action', event.event.action);
  return {
    kind: 'event',
    eyebrow: event.event.category,
    title: message,
    meta,
    output: outputPreview(event),
  };
}
