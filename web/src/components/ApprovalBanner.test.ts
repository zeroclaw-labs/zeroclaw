import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { Window } from 'happy-dom';
import { createJiti } from 'jiti';
import type { PendingApproval } from '../types/api.ts';

test('approval preview renders proposed markup and attachment markers literally', async () => {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __ZEROCLAW_BASE__: '' },
  });
  const dom = new Window();
  try {
    const jiti = createJiti(import.meta.url, {
      fsCache: false, moduleCache: false, tsconfigPaths: true,
      jsx: { runtime: 'automatic' },
    });
    const { default: ApprovalBanner } = await jiti.import<typeof import('./ApprovalBanner.tsx')>('./ApprovalBanner.tsx');
    const summary = '[FILE:report.txt] <img src="https://example.invalid/x" onerror="alert(1)"> **literal** & <!channel>';
    const pending: PendingApproval = {
      requestId: 'strict-test', toolName: 'session_prompt_set',
      argumentsSummary: summary, timeoutSecs: 300,
      receivedAt: Date.now(), allowAlways: false,
    };
    for (const allowAlways of [false, true]) {
      dom.document.body.innerHTML = renderToStaticMarkup(createElement(ApprovalBanner, {
        pending: { ...pending, allowAlways }, onRespond: () => {},
      }));
      const preview = dom.document.querySelector('pre');
      assert.equal(preview?.textContent, summary);
      assert.equal(preview?.children.length, 0);
      assert.equal(dom.document.querySelectorAll('img, script, a').length, 0);
      assert.equal(dom.document.querySelectorAll('button').length, allowAlways ? 3 : 2);
    }
  } finally {
    await dom.happyDOM.close();
    if (previousWindow) {
      Object.defineProperty(globalThis, 'window', previousWindow);
    } else {
      delete (globalThis as { window?: unknown }).window;
    }
  }
});
