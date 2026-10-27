import assert from 'node:assert/strict';
import test, { beforeEach } from 'node:test';
import {
  loadCodeSelection,
  resumeCodeSession,
  saveCodeSelection,
} from './codeSelection';
import type { Session } from '@/types/api';

const values = new Map<string, string>();
Object.assign(globalThis, {
  localStorage: {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  },
});
beforeEach(() => values.clear());
const entry = (
  id: string,
  agent: string,
  date: string,
  surface = 'zerocode_code',
): Session & { interaction_surface: string } => ({
  session_id: id,
  session_key: id,
  agent_alias: agent,
  channel_id: null,
  created_at: date,
  last_activity: date,
  message_count: 2,
  name: '',
  interaction_surface: surface,
});
const history = [
  entry('old', 'builder', '2026-09-01'),
  entry('new', 'reviewer', '2026-10-01'),
];

test('reopening Code restores the selected session across agents, not the newest session', () => {
  saveCodeSelection('builder', 'old');
  assert.deepEqual(loadCodeSelection(), { agent: 'builder', session: 'old' });
  assert.equal(
    resumeCodeSession(history, ['builder', 'reviewer'])?.session_id,
    'old',
  );
});
test('deleted selections fall back to the latest available code session', () => {
  saveCodeSelection('builder', 'deleted');
  assert.equal(
    resumeCodeSession(history, ['builder', 'reviewer'])?.session_id,
    'new',
  );
});
test('a stored ID cannot resume another agent or a non-code surface', () => {
  saveCodeSelection('reviewer', 'old');
  const all = [...history, entry('chat', 'reviewer', '2026-10-02', 'web_chat')];
  assert.equal(
    resumeCodeSession(all, ['builder', 'reviewer'])?.session_id,
    'new',
  );
});
test('disabled or removed agents are excluded from restoration', () => {
  saveCodeSelection('reviewer', 'new');
  assert.equal(resumeCodeSession(history, ['builder'])?.session_id, 'old');
  assert.equal(resumeCodeSession(history, []), undefined);
});
test('malformed browser preferences do not block opening Code', () => {
  values.set('zeroclaw-code-selection', '{broken');
  assert.equal(loadCodeSelection(), null);
  assert.equal(
    resumeCodeSession(history, ['builder', 'reviewer'])?.session_id,
    'new',
  );
});
