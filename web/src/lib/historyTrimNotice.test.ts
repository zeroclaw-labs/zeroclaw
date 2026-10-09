import assert from 'node:assert/strict';
import test from 'node:test';

import { formatHistoryTrimmedNotice } from './historyTrimNotice.ts';

const strings: Record<string, string> = {
  'agent.history_trimmed': 'Trimmed: {reason} ({dropped} messages dropped; {kept} turns kept).',
  'agent.history_trimmed_turns': 'Trimmed: {reason} ({dropped} older {dropped_unit} dropped; {kept} {kept_unit} kept).',
  'agent.history_trimmed_turn_singular': 'turn',
  'agent.history_trimmed_turn_plural': 'turns',
  'agent.history_trimmed_unknown_reason': 'history limit exceeded',
};
const translate = (key: string) => strings[key] ?? key;

test('formats whole-turn accounting with singular grammar', () => {
  const notice = formatHistoryTrimmedNotice({
    type: 'history_trimmed',
    dropped_messages: 2,
    dropped_turns: 1,
    kept_turns: 1,
    reason: 'turn limit',
  }, translate);

  assert.equal(notice, 'Trimmed: turn limit (1 older turn dropped; 1 turn kept).');
});

test('falls back to legacy message accounting when dropped_turns is absent', () => {
  const notice = formatHistoryTrimmedNotice({
    type: 'history_trimmed',
    dropped_messages: 4,
    kept_turns: 2,
    reason: 'message limit',
  }, translate);

  assert.equal(notice, 'Trimmed: message limit (4 messages dropped; 2 turns kept).');
});
