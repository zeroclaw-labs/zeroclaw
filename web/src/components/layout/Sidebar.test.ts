import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement, type Ref } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter } from 'react-router-dom';
import { railAsideStyle, railNavClassName } from './sidebarRail.ts';
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

test('SidebarNavLink forwards a ref prop to the underlying NavLink', () => {
  // The rail tooltip positions itself from
  // `linkRef.current.getBoundingClientRect()`, so the ref passed to
  // `SidebarNavLink` must reach the rendered `<a>`. React 19 passes ref as a
  // regular prop; the component must spread it through.
  const ref: Ref<HTMLAnchorElement> = { current: null };
  const el = SidebarNavLink({
    activePath: '/config',
    to: '/config/agents',
    ref,
    children: 'Agent',
  });
  assert.equal(el.props.ref, ref);
});

test('desktop rail relies on the portal, not an overflow-x mask, for horizontal overflow', () => {
  // The rail tooltip is portaled out of the nav subtree, so `overflow-y:
  // auto` alone must keep the rail at `scrollWidth === clientWidth`. An
  // `overflow-x` hidden/clip added here masks the symptom at the container
  // instead of removing the overflowing content, and it reintroduces the
  // band-aid that #8791 reviewers rejected.
  assert.ok(railNavClassName.includes('overflow-y-auto'));
  assert.ok(
    !railNavClassName.includes('overflow-x'),
    'rail nav must not carry an overflow-x class',
  );
  assert.ok(
    !('overflowX' in railAsideStyle),
    'rail aside must not carry an overflowX inline style',
  );
});
