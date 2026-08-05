import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { SidebarNavLink } from './SidebarNavLink.ts';

function renderSidebarLinks(pathname: string, activePath: string): string {
  const nav = (variant: string) =>
    createElement(
      'nav',
      { 'aria-label': variant, key: variant },
      createElement(
        SidebarNavLink,
        { activePath, to: '/config' },
        'Config',
      ),
      createElement(
        SidebarNavLink,
        { activePath, to: '/config/agents' },
        'Agent',
      ),
    );

  return renderToStaticMarkup(
    createElement(
      MemoryRouter,
      { initialEntries: [pathname] },
      createElement('div', null, nav('desktop'), nav('mobile')),
    ),
  );
}

function currentLinks(html: string): string[] {
  return Array.from(html.matchAll(/<a\b[^>]*aria-current="page"[^>]*>/g), ([link]) => link);
}

test('agent config routes select only Agent in both sidebar variants', () => {
  for (const pathname of ['/config/agents', '/config/agents/zeroclaw_agent']) {
    const links = currentLinks(renderSidebarLinks(pathname, '/config/agents'));

    assert.equal(links.length, 2);
    assert.ok(links.every((link) => link.includes('href="/config/agents"')));
  }
});

test('other config routes select only Config in both sidebar variants', () => {
  const links = currentLinks(renderSidebarLinks('/config/providers', '/config'));

  assert.equal(links.length, 2);
  assert.ok(links.every((link) => link.includes('href="/config"')));
});
