import assert from 'node:assert/strict';
import test from 'node:test';

import {
  buildHistoryTrimmedNotice,
  type HistoryTrimmedNoticeMessage,
} from './historyTrimNotices.logic.ts';

// English strings mirrored from `@/lib/i18n` so the branch logic is tested
// without a browser/React harness.
const en: Record<string, string> = {
  'agent.history_trimmed':
    'Earlier conversation history was trimmed: {reason} ({dropped} messages dropped; {kept} turns kept).',
  'agent.history_trimmed_tokens':
    'Earlier conversation history was trimmed from approximately {before} to {after} tokens: {reason}; {dropped} messages dropped and {kept} turns kept.',
  'agent.history_trimmed_tokens_budget_clause': ' (configured token budget: {budget})',
  'agent.history_trimmed_tokens_source_provider': 'provider-reported',
  'agent.history_trimmed_tokens_source_estimate': 'estimated',
  'agent.history_trimmed_tokens_source_calibrated': 'provider + estimate',
  'agent.history_trimmed_tokens_sources': '({before} before; {after} after)',
  'agent.history_trimmed_unknown_reason': 'history limit exceeded',
};
const t = (key: string) => en[key] ?? key;

test('recovery below a large configured budget renders counts with the recovery phrase, not the budget as target', () => {
  const text = buildHistoryTrimmedNotice(
    {
      reason: 'context window overflow recovery',
      dropped_messages: 4,
      kept_turns: 2,
      token_budget: 500000,
      tokens_before: 612000,
      tokens_after: 117000,
    } satisfies HistoryTrimmedNoticeMessage,
    t,
  );
  assert.match(text, /612000/);
  assert.match(text, /117000/);
  assert.match(text, /context window overflow recovery/);
  assert.match(text, /configured token budget: 500000/);
  assert.ok(!/against a \d+/.test(text), 'must not present the configured limit as the trim target');
});

test('recovery with enforcement disabled still renders valid counts without a budget clause', () => {
  const text = buildHistoryTrimmedNotice(
    {
      reason: 'context window overflow recovery',
      dropped_messages: 4,
      kept_turns: 2,
      token_budget: undefined,
      tokens_before: 612000,
      tokens_after: 117000,
    } satisfies HistoryTrimmedNoticeMessage,
    t,
  );
  assert.match(text, /612000/);
  assert.match(text, /117000/);
  assert.match(text, /context window overflow recovery/);
  assert.ok(!text.includes('budget'), 'no configured budget means no budget clause');
});

test('configured-budget trim keeps the neutral budget clause and shows the reason', () => {
  const text = buildHistoryTrimmedNotice(
    {
      reason: 'context token budget exceeded',
      dropped_messages: 12,
      kept_turns: 33,
      token_budget: 100000,
      tokens_before: 612000,
      tokens_after: 117000,
    } satisfies HistoryTrimmedNoticeMessage,
    t,
  );
  assert.match(text, /context token budget exceeded/);
  assert.match(text, /configured token budget: 100000/);
  assert.ok(!/against a \d+/.test(text));
});

test('message-limit trim without token accounting keeps the count-only fallback', () => {
  const text = buildHistoryTrimmedNotice(
    {
      reason: 'history message limit exceeded',
      dropped_messages: 12,
      kept_turns: 3,
    } satisfies HistoryTrimmedNoticeMessage,
    t,
  );
  assert.match(text, /history message limit exceeded/);
  assert.match(text, /12 messages dropped/);
  assert.match(text, /3 turns kept/);
});
