import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

type CatalogLoadWarning = {
  source: 'agent' | 'cli';
  message: string;
};

const warnings: CatalogLoadWarning[] = [
  { source: 'agent', message: 'agent catalog unavailable' },
  { source: 'cli', message: 'CLI catalog unavailable' },
];

async function renderPanel({
  panelWarnings,
  retryDisabled,
}: {
  panelWarnings: CatalogLoadWarning[];
  retryDisabled: boolean;
}): Promise<string> {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __ZEROCLAW_BASE__: '' },
  });
  const { ToolCatalogWarningPanel } = await import('./ToolCatalogWarningPanel.ts');
  const html = renderToStaticMarkup(
    createElement(ToolCatalogWarningPanel, {
      warnings: panelWarnings,
      onRetry: () => {},
      retryDisabled,
    }),
  );
  delete (globalThis as { window?: unknown }).window;
  return html;
}

test('partial catalog warning renders source failures and retry action', async () => {
  const html = await renderPanel({ panelWarnings: warnings, retryDisabled: false });

  assert.match(html, /Some tools could not be loaded\./);
  assert.match(html, /Agent tools: agent catalog unavailable/);
  assert.match(html, /CLI tools: CLI catalog unavailable/);
  assert.match(html, /aria-label="Retry loading tools"/);
  // Inserted asynchronously after the catalog settles, so it must announce to
  // assistive tech as a polite live region.
  assert.match(html, /role="status"/);
  assert.match(html, /aria-live="polite"/);
});

test('partial catalog retry button can render disabled while reloading', async () => {
  const html = await renderPanel({ panelWarnings: [warnings[0]!], retryDisabled: true });

  assert.match(html, /disabled=""/);
});
