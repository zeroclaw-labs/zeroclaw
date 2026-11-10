import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { ConversationCostSummary } from '../lib/api.ts';

Object.assign(globalThis, { React, IS_REACT_ACT_ENVIRONMENT: true, localStorage: { getItem: () => 'en' }, window: { location: { protocol: 'http:', host: 'localhost' } } });
Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { language: 'en-US' } });
const { ConversationCost } = await import('./ConversationCost.tsx');

test('conversation cost renders available, partial, unavailable and tiny amounts with reasons', async () => {
  const scenarios: Array<[ConversationCostSummary | null, string]> = [
    [null, 'Cost unavailable'],
    [{ estimated_cost_usd: 0, status: 'estimated', reasons: [] }, 'Est. $0.0000'],
    [{ estimated_cost_usd: 3.42, status: 'partial', reasons: ['missing_usage'] }, 'Est. $3.4200 · incomplete'],
    [{ estimated_cost_usd: 0.0000001, status: 'estimated', reasons: [] }, 'Est. <$0.0001'],
    [{ estimated_cost_usd: null, status: 'unavailable', reasons: ['tracking_disabled'] }, 'Cost unavailable'],
  ];
  for (const [summary, expected] of scenarios) {
    let root!: ReactTestRenderer;
    await act(async () => { root = create(React.createElement(ConversationCost, { summary })); });
    const span = root.root.findByType('span');
    assert.equal(span.children.join(''), expected);
    assert.match(span.props.title, /recorded conversation usage/);
    if (summary?.reasons.includes('missing_usage')) assert.match(span.props.title, /missing or incomplete/);
    await act(async () => root.unmount());
  }
});
