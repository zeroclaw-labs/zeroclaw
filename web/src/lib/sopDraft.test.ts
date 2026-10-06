import assert from 'node:assert/strict';
import test from 'node:test';
import { sopProposalSource } from './sopDraft.ts';

const source = JSON.stringify({
  name: 'Example workflow',
  triggers: [{ type: 'manual' }],
  steps: [],
});

test('finds a complete SOP proposal after unrelated JSON examples', () => {
  const message =
    'Example input:\n```json\n{"message":"hello"}\n```\nProposed SOP:\n```json\n' +
    source +
    '\n```';
  assert.equal(sopProposalSource(message)?.trim(), source);
});

test('partial streams and malformed definitions cannot produce an apply action', () => {
  for (const message of [
    '```json\n' + source,
    '```json\n{"name":"Example", "steps":\n```',
    '```json\n{"name":"Example", "steps":{}, "triggers":[]}\n```',
    '```json\nnull\n```',
    '```json\n[]\n```',
    source,
  ])
    assert.equal(sopProposalSource(message), null);
});

test('accepts an unlabelled complete JSON block', () => {
  assert.equal(sopProposalSource('```\n' + source + '\n```')?.trim(), source);
});
