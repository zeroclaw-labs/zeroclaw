import assert from 'node:assert/strict';
import test from 'node:test';

import { ThoughtChunkBuffer } from './thought-stream.ts';

test('consecutive thought chunks drain as one transcript paragraph', () => {
  const buffer = new ThoughtChunkBuffer();

  buffer.append('I');
  buffer.append(' need');
  buffer.append(' more context.');

  assert.equal(buffer.take(), 'I need more context.');
  assert.equal(buffer.take(), null);
});

test('a boundary separates consecutive thought paragraphs', () => {
  const buffer = new ThoughtChunkBuffer();

  buffer.append('First');
  buffer.append(' thought.');
  assert.equal(buffer.take(), 'First thought.');

  buffer.append('Second thought.');
  assert.equal(buffer.take(), 'Second thought.');
});

test('draining drops whitespace-only chunks without leaking them forward', () => {
  const buffer = new ThoughtChunkBuffer();

  buffer.append('  \n');
  assert.equal(buffer.take(), null);

  buffer.append('Visible');
  assert.equal(buffer.take(), 'Visible');
});

test('clearing discards a pending thought when the session resets', () => {
  const buffer = new ThoughtChunkBuffer();

  buffer.append('stale session thought');
  buffer.clear();

  assert.equal(buffer.take(), null);
});
