import assert from 'node:assert/strict';
import test from 'node:test';

import {
  classifyCompletion,
  initialTurnStreamState,
  reduceTurnFrame,
  type CompletionOutcome,
  type TurnStreamFrame,
} from './turnStream.logic.ts';

/** Fold a whole frame sequence, collecting every completion the turns emit.
 *  Returns the final state so tests can assert cross-turn reset invariants. */
function runFrames(frames: TurnStreamFrame[]) {
  let state = initialTurnStreamState();
  const completions: CompletionOutcome[] = [];
  for (const frame of frames) {
    const result = reduceTurnFrame(state, frame);
    state = result.state;
    if (result.completion) completions.push(result.completion);
  }
  return { state, completions };
}

// ── The reasoning-only bug this PR fixes ────────────────────────────────────

test('reasoning-only turn commits with empty content and captured thinking', () => {
  // GLM/Qwen/DeepSeek: whole answer arrives as reasoning deltas, content is
  // empty, no tool calls. Before the fix this produced no bubble at all.
  const { completions } = runFrames([
    { type: 'thinking', content: 'The user asks why ' },
    { type: 'thinking', content: 'the 21:00 shutdown ' },
    { type: 'thinking', content: 'did not run.' },
    { type: 'chunk', content: '' },
    { type: 'done', full_response: '' },
  ]);
  assert.equal(completions.length, 1);
  assert.deepEqual(completions[0], {
    kind: 'commit',
    content: '',
    thinking: 'The user asks why the 21:00 shutdown did not run.',
  });
});

// ── Genuinely empty clean completion → exactly one diagnostic ───────────────

test('empty clean completion emits exactly one diagnostic', () => {
  const { completions } = runFrames([{ type: 'done', full_response: '' }]);
  assert.equal(completions.length, 1);
  assert.deepEqual(completions[0], { kind: 'diagnostic' });
});

// ── Tool-only completion → no diagnostic (cards are the record) ─────────────

test('tool-only completion skips: no diagnostic when tools ran', () => {
  const { completions } = runFrames([
    { type: 'tool_call', hasName: true },
    { type: 'done', full_response: '' },
  ]);
  assert.equal(completions.length, 1);
  assert.deepEqual(completions[0], { kind: 'skip' });
});

// ── Normal visible answer → commit with the streamed text ───────────────────

test('normal content commits the concatenated stream', () => {
  const { completions } = runFrames([
    { type: 'chunk', content: 'Round 1: 2 + 3 ' },
    { type: 'chunk', content: '+ 4 = 9' },
    { type: 'done' },
  ]);
  assert.equal(completions.length, 1);
  assert.deepEqual(completions[0], {
    kind: 'commit',
    content: 'Round 1: 2 + 3 + 4 = 9',
    thinking: undefined,
  });
});

// ── #6702: whitespace-only content alongside a tool_call → skip ─────────────

test('whitespace-only content with a tool call skips (issue #6702)', () => {
  const { completions } = runFrames([
    { type: 'chunk', content: '\n\n' },
    { type: 'tool_call', hasName: true },
    { type: 'done', full_response: '' },
  ]);
  assert.equal(completions.length, 1);
  assert.deepEqual(completions[0], { kind: 'skip' });
});

// ── A nameless telemetry tool_call must not flip hadToolCall (#7151) ────────

test('nameless tool_call frame does not count as a tool call', () => {
  // Empty completion after only a nameless (telemetry) tool_call must still
  // diagnose — the turn genuinely produced nothing.
  const { completions } = runFrames([
    { type: 'tool_call', hasName: false },
    { type: 'done', full_response: '' },
  ]);
  assert.deepEqual(completions[0], { kind: 'diagnostic' });
});

// ── chunk_reset snapshots thinking before the authoritative done ────────────

test('chunk_reset captures thinking so the commit keeps it', () => {
  const { completions } = runFrames([
    { type: 'thinking', content: 'reasoning here' },
    { type: 'chunk', content: 'draft that gets reset' },
    { type: 'chunk_reset' },
    { type: 'done', full_response: 'final answer' },
  ]);
  assert.deepEqual(completions[0], {
    kind: 'commit',
    content: 'final answer',
    thinking: 'reasoning here',
  });
});

// ── The invariant the reviewer called out: state resets across turns ────────

test('per-turn state resets between turns', () => {
  // Turn 1: tool-only (skip). Turn 2: reasoning-only (commit). Turn 2 must NOT
  // see turn 1's hadToolCall or leftover buffers.
  const { state, completions } = runFrames([
    // turn 1
    { type: 'chunk', content: '\n\n' },
    { type: 'tool_call', hasName: true },
    { type: 'done', full_response: '' },
    // turn 2
    { type: 'thinking', content: 'fresh reasoning' },
    { type: 'done', full_response: '' },
  ]);
  assert.deepEqual(completions, [
    { kind: 'skip' },
    { kind: 'commit', content: '', thinking: 'fresh reasoning' },
  ]);
  // After the second completion the state is fully fresh.
  assert.deepEqual(state, initialTurnStreamState());
});

test('empty second turn diagnoses instead of inheriting turn 1 tool call', () => {
  // Regression guard: if hadToolCall leaked across turns, this empty second
  // turn would wrongly skip instead of diagnosing.
  const { completions } = runFrames([
    { type: 'tool_call', hasName: true },
    { type: 'done', full_response: 'answer one' },
    { type: 'done', full_response: '' },
  ]);
  assert.deepEqual(completions, [
    { kind: 'commit', content: 'answer one', thinking: undefined },
    { kind: 'diagnostic' },
  ]);
});

test('error then new turn discards thinking captured before the error', () => {
  // Regression sequence from review: the first turn snapshots reasoning, then
  // fails. Starting a new turn must not let that stale reasoning turn an empty
  // completion into a reasoning-only assistant message.
  const { state, completions } = runFrames([
    { type: 'thinking', content: 'stale reasoning' },
    { type: 'chunk_reset' },
    { type: 'error' },
    { type: 'turn_start' },
    { type: 'done', full_response: '' },
  ]);
  assert.deepEqual(completions, [{ kind: 'diagnostic' }]);
  assert.deepEqual(state, initialTurnStreamState());
});

// ── classifyCompletion fallback chain (full_response ?? content ?? pending) ──

test('classifyCompletion falls back to frame content then pending stream', () => {
  // full_response present wins.
  assert.deepEqual(
    classifyCompletion(
      { pendingContent: 'streamed', pendingThinking: '', capturedThinking: '', hadToolCall: false },
      { full_response: 'authoritative' },
    ),
    { kind: 'commit', content: 'authoritative', thinking: undefined },
  );
  // no full_response → frame content.
  assert.deepEqual(
    classifyCompletion(
      { pendingContent: 'streamed', pendingThinking: '', capturedThinking: '', hadToolCall: false },
      { content: 'frame body' },
    ),
    { kind: 'commit', content: 'frame body', thinking: undefined },
  );
  // neither → the live-streamed pending content.
  assert.deepEqual(
    classifyCompletion(
      { pendingContent: 'streamed', pendingThinking: '', capturedThinking: '', hadToolCall: false },
      {},
    ),
    { kind: 'commit', content: 'streamed', thinking: undefined },
  );
});

test('captured thinking takes precedence over pending thinking', () => {
  assert.deepEqual(
    classifyCompletion(
      { pendingContent: '', pendingThinking: 'pending', capturedThinking: 'captured', hadToolCall: false },
      { full_response: '' },
    ),
    { kind: 'commit', content: '', thinking: 'captured' },
  );
});
