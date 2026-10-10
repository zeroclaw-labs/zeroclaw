import assert from 'node:assert/strict';
import test from 'node:test';
import { Window } from 'happy-dom';
import { createJiti } from 'jiti';
import * as React from 'react';
import { act } from 'react';
import type { CronJob } from '../types/api.ts';

interface PatchCall {
  id: string;
  body: Record<string, unknown>;
}

function cronJob(
  id: string,
  name: string,
  schedule: CronJob['schedule'],
  expression: string,
): CronJob {
  return {
    id,
    name,
    expression,
    command: 'echo before',
    prompt: null,
    job_type: 'shell',
    schedule,
    enabled: true,
    delivery: { mode: 'none', channel: null, to: null, best_effort: true },
    delete_after_run: false,
    uses_memory: true,
    session_target: 'isolated',
    model: null,
    allowed_tools: null,
    source: 'manual',
    agent_alias: 'default',
    created_at: '2026-10-01T00:00:00Z',
    next_run: '2026-10-06T09:00:00Z',
    last_run: null,
    last_status: null,
    last_output: null,
  };
}

const jsonResponse = (body: unknown): Response =>
  ({
    ok: true,
    status: 200,
    statusText: 'OK',
    text: async () => JSON.stringify(body),
  }) as Response;

function findEditButton(document: Document, jobName: string): HTMLButtonElement {
  const row = Array.from(document.querySelectorAll('tr')).find((candidate) =>
    candidate.textContent?.includes(jobName),
  );
  assert.ok(row, `expected table row for ${jobName}`);
  const button = row.querySelector<HTMLButtonElement>('button[aria-label="Edit"]');
  assert.ok(button, `expected Edit button for ${jobName}`);
  return button;
}

function findNameInput(document: Document): HTMLInputElement {
  const label = Array.from(document.querySelectorAll('label')).find((candidate) =>
    candidate.textContent?.toLowerCase().includes('name'),
  );
  assert.ok(label, 'expected the job name label');
  const input = label.parentElement?.querySelector('input');
  assert.ok(input, 'expected the job name input');
  return input;
}

function setInputValue(input: HTMLInputElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(
    Object.getPrototypeOf(input),
    'value',
  )?.set;
  assert.ok(setter, 'expected the native input value setter');
  setter.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
  input.dispatchEvent(new Event('change', { bubbles: true }));
}

function flushEffects(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

test('editing an unsupported schedule sends only unrelated cron fields', async () => {
  const originalFetch = globalThis.fetch;
  const domWindow = new Window({ url: 'http://localhost:42617/' });
  const document = domWindow.document as unknown as Document;
  const container = document.createElement('div');
  document.body.appendChild(container);
  const globalKeys = [
    'window',
    'document',
    'navigator',
    'localStorage',
    'HTMLElement',
    'HTMLInputElement',
    'HTMLSelectElement',
    'HTMLButtonElement',
    'Node',
    'Element',
    'Event',
    'MouseEvent',
    'KeyboardEvent',
    'Text',
    'getComputedStyle',
    'requestAnimationFrame',
    'cancelAnimationFrame',
    'IS_REACT_ACT_ENVIRONMENT',
  ] as const;
  const previous = new Map<string, PropertyDescriptor | undefined>();
  for (const key of globalKeys) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
  }

  Object.defineProperties(globalThis, {
    window: { configurable: true, value: domWindow, writable: true },
    document: { configurable: true, value: domWindow.document, writable: true },
    navigator: { configurable: true, value: domWindow.navigator, writable: true },
    localStorage: { configurable: true, value: domWindow.localStorage, writable: true },
    HTMLElement: { configurable: true, value: domWindow.HTMLElement, writable: true },
    HTMLInputElement: { configurable: true, value: domWindow.HTMLInputElement, writable: true },
    HTMLSelectElement: { configurable: true, value: domWindow.HTMLSelectElement, writable: true },
    HTMLButtonElement: { configurable: true, value: domWindow.HTMLButtonElement, writable: true },
    Node: { configurable: true, value: domWindow.Node, writable: true },
    Element: { configurable: true, value: domWindow.Element, writable: true },
    Event: { configurable: true, value: domWindow.Event, writable: true },
    MouseEvent: { configurable: true, value: domWindow.MouseEvent, writable: true },
    KeyboardEvent: { configurable: true, value: domWindow.KeyboardEvent, writable: true },
    Text: { configurable: true, value: domWindow.Text, writable: true },
    getComputedStyle: {
      configurable: true,
      value: domWindow.getComputedStyle.bind(domWindow),
      writable: true,
    },
    requestAnimationFrame: {
      configurable: true,
      value: (callback: FrameRequestCallback) =>
        setTimeout(() => callback(Date.now()), 0),
      writable: true,
    },
    cancelAnimationFrame: {
      configurable: true,
      value: (id: number) => clearTimeout(id),
      writable: true,
    },
    IS_REACT_ACT_ENVIRONMENT: { configurable: true, value: true, writable: true },
  });

  const fixtures = [
    cronJob(
      'six-field',
      'six-field schedule',
      { kind: 'cron', expr: '0 0 9 * * 1-5' },
      '0 0 9 * * 1-5',
    ),
    cronJob(
      'seven-field',
      'seven-field schedule',
      { kind: 'cron', expr: '0 0 9 * * 1-5 2027' },
      '0 0 9 * * 1-5 2027',
    ),
    cronJob(
      'one-shot',
      'one-shot schedule',
      { kind: 'at', at: '2030-01-01T00:00:00Z' },
      'at 2030-01-01T00:00:00Z',
    ),
    cronJob(
      'interval',
      'interval schedule',
      { kind: 'every', every_ms: 3_600_000 },
      'every 3600000ms',
    ),
  ];
  let jobs = fixtures;
  const patchCalls: PatchCall[] = [];
  globalThis.fetch = async (input, init) => {
    const rawUrl =
      typeof input === 'string'
        ? input
        : input instanceof URL
          ? input.toString()
          : input.url;
    const url = new URL(rawUrl, 'http://localhost:42617/');
    if (url.pathname === '/api/cron' && (init?.method ?? 'GET') === 'GET') {
      return jsonResponse(jobs);
    }
    if (url.pathname === '/api/cron/settings') {
      return jsonResponse({ enabled: true, catch_up_on_startup: false, max_run_history: 20 });
    }
    if (url.pathname === '/api/quickstart/state') {
      return jsonResponse({ agents: ['default'] });
    }
    if (url.pathname === '/api/config/prop') {
      return jsonResponse({ path: url.searchParams.get('path') ?? '', value: '<unset>' });
    }
    const patchMatch = url.pathname.match(/^\/api\/cron\/([^/]+)$/);
    if (patchMatch && init?.method === 'PATCH') {
      const id = decodeURIComponent(patchMatch[1] ?? '');
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      patchCalls.push({ id, body });
      const existing = jobs.find((job) => job.id === id);
      assert.ok(existing, `unexpected patch for ${id}`);
      const updated = {
        ...existing,
        ...(typeof body.name === 'string' ? { name: body.name } : {}),
        ...(typeof body.command === 'string' ? { command: body.command } : {}),
        ...(typeof body.schedule === 'string'
          ? { expression: body.schedule, schedule: { kind: 'cron', expr: body.schedule } }
          : {}),
      } as CronJob;
      jobs = jobs.map((job) => (job.id === id ? updated : job));
      return jsonResponse(updated);
    }
    return {
      ok: false,
      status: 404,
      statusText: 'Not Found',
      text: async () => JSON.stringify({ error: `Unexpected request: ${init?.method ?? 'GET'} ${url.pathname}` }),
    } as Response;
  };

  let root: { render: (children: React.ReactNode) => void; unmount: () => void } | undefined;
  try {
    const jiti = createJiti(import.meta.url, {
      fsCache: false,
      moduleCache: false,
      jsx: { runtime: 'automatic' },
      alias: { '@': new URL('../', import.meta.url).pathname.replace(/\/$/, '') },
    });
    const module = await jiti.import<typeof import('./Cron.tsx')>('./Cron.tsx');
    const Cron = module.default as React.ComponentType;
    const { createRoot } = await import('react-dom/client');
    root = createRoot(container as unknown as HTMLElement);
    await act(async () => {
      root?.render(React.createElement(Cron));
      await flushEffects();
    });

    for (const fixture of fixtures) {
      await act(async () => {
        findEditButton(document, fixture.name ?? '').click();
        await flushEffects();
      });
      const scheduleText =
        fixture.schedule.kind === 'cron'
          ? fixture.schedule.expr
          : fixture.schedule.kind === 'at'
            ? fixture.schedule.at
            : String(fixture.schedule.every_ms);
      assert.ok(
        Array.from(document.querySelectorAll('code')).some(
          (node) => node.textContent?.trim() === scheduleText,
        ),
        `expected read-only schedule ${scheduleText}`,
      );

      const nameInput = findNameInput(document);
      await act(async () => {
        setInputValue(nameInput, `${fixture.name} edited`);
      });
      const save = Array.from(document.querySelectorAll('button')).find(
        (button) => button.textContent?.trim() === 'Save',
      );
      assert.ok(save, 'expected the save button');
      await act(async () => {
        save.click();
        await flushEffects();
      });

      const call = patchCalls[patchCalls.length - 1];
      assert.ok(call, `expected a PATCH for ${fixture.id}`);
      assert.equal(call.id, fixture.id);
      assert.equal(call.body.name, `${fixture.name} edited`);
      assert.equal(call.body.command, fixture.command);
      assert.equal('schedule' in call.body, false);
      assert.deepEqual(
        jobs.find((job) => job.id === fixture.id)?.schedule,
        fixture.schedule,
        `unrelated edit must retain ${fixture.name}'s stored schedule`,
      );
    }
  } finally {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    globalThis.fetch = originalFetch;
    for (const key of globalKeys) {
      const descriptor = previous.get(key);
      if (descriptor) {
        Object.defineProperty(globalThis, key, descriptor);
      } else {
        Reflect.deleteProperty(globalThis, key);
      }
    }
    domWindow.close();
  }
});
