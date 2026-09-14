import assert from 'node:assert/strict';
import test from 'node:test';

import {
  initialTerminalExplanationState,
  reduceTerminalFrame,
  type TerminalFrame,
  bannerForErrorFrame,
  contextExhaustedBubblePresentation,
  type TerminalRender,
} from './terminalExplanation.logic.ts';

/** Fold a frame sequence, collecting everything the turn would render. */
function runFrames(frames: TerminalFrame[]) {
  let state = initialTerminalExplanationState();
  const rendered: TerminalRender[] = [];
  for (const frame of frames) {
    const result = reduceTerminalFrame(state, frame);
    state = result.state;
    if (result.render.kind !== 'none') rendered.push(result.render);
  }
  return { state, rendered };
}

const NOTICE =
  "*Turn stopped: the conversation exceeded the model's context window and could not be " +
  'reduced further. Start a new conversation or shorten the request.*';

// ── The bug this PR fixes ───────────────────────────────────────────────────

test('context exhaustion renders the localized notice on the live turn', () => {
  // Before the fix the gateway sent only `error`, so the mounted transcript
  // showed a generic provider-error bubble and the real reason appeared only
  // after a reload (#8758).
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted', notice: NOTICE },
    { type: 'error' },
  ]);

  assert.deepEqual(rendered, [{ kind: 'notice', content: NOTICE }]);
});

test('an explained turn renders exactly one terminal explanation', () => {
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted', notice: NOTICE },
    { type: 'error' },
  ]);

  assert.equal(rendered.length, 1, 'the generic error bubble must not restate the same stop');
  assert.deepEqual(rendered[0], { kind: 'notice', content: NOTICE });
});

// ── Negative controls: the suppression must not overreach ───────────────────

test('an ordinary failed turn still renders the generic error bubble', () => {
  const { rendered } = runFrames([{ type: 'turn_start' }, { type: 'error' }]);

  assert.deepEqual(rendered, [{ kind: 'error' }]);
});

test('a later unrelated failure is not swallowed by an earlier notice', () => {
  // Regression: a sticky flag would let turn 1's context-exhaustion notice
  // suppress turn 2's genuine error, leaving that turn silent.
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted', notice: NOTICE },
    { type: 'error' },
    { type: 'turn_start' },
    { type: 'error' },
  ]);

  assert.deepEqual(rendered, [{ kind: 'notice', content: NOTICE }, { kind: 'error' }]);
});

test('a notice with no following error does not leak into the next turn', () => {
  // The explanation is normally consumed by the error frame that follows it.
  // If that frame never arrives — dropped socket, or the user sends again
  // first — only the `turn_start` reset stops the stale flag from swallowing
  // the *next* turn's genuine error.
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted', notice: NOTICE },
    // no `error` for this turn
    { type: 'turn_start' },
    { type: 'error' },
  ]);

  assert.deepEqual(rendered, [{ kind: 'notice', content: NOTICE }, { kind: 'error' }]);
});

test('back-to-back error frames after one notice still surface the second', () => {
  // The explanation is consumed by the error it explains, not held open.
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted', notice: NOTICE },
    { type: 'error' },
    { type: 'error' },
  ]);

  assert.deepEqual(rendered, [{ kind: 'notice', content: NOTICE }, { kind: 'error' }]);
});

test('a notice frame with no text falls back to the generic error bubble', () => {
  // Defensive: a malformed/truncated frame must not silence the failure.
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted' },
    { type: 'error' },
  ]);

  assert.deepEqual(rendered, [{ kind: 'error' }]);
});

test('state resets across turns so no explanation leaks forward', () => {
  const { state } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted', notice: NOTICE },
    { type: 'error' },
  ]);

  assert.equal(state.explained, false);
});

// ── Banner arbitration (#8758 follow-up) ─────────────────────────────
//
// Regression cover for a real escape: the bubble was correctly suppressed
// while the banner was keyed off `msg.code` alone, so an explained turn still
// flashed the raw provider text as "Configuration error". Caught in a browser,
// not by the wire probe — both frames are on the wire by design, so only the
// render decision can distinguish the bug.

test('an explained turn raises no banner even though the code is PROVIDER_ERROR', () => {
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted', notice: NOTICE },
    { type: 'error' },
  ]);
  // The error frame rendered nothing...
  assert.deepEqual(rendered, [{ kind: 'notice', content: NOTICE }]);

  // ...so the banner must stay silent for the very code context exhaustion
  // travels under. This is the assertion that fails on the buggy version.
  const outcome = reduceTerminalFrame({ explained: true }, { type: 'error' });
  assert.deepEqual(bannerForErrorFrame(outcome.render, 'PROVIDER_ERROR'), { kind: 'none' });
});

test('an unexplained provider failure still raises the configuration banner', () => {
  // The guard must not swallow genuine misconfiguration (dead endpoint, bad key).
  const outcome = reduceTerminalFrame({ explained: false }, { type: 'error' });
  assert.deepEqual(outcome.render, { kind: 'error' });
  assert.deepEqual(bannerForErrorFrame(outcome.render, 'PROVIDER_ERROR'), {
    kind: 'configuration',
  });
  assert.deepEqual(bannerForErrorFrame(outcome.render, 'AUTH_ERROR'), { kind: 'configuration' });
});

test('malformed-message codes still raise the message banner', () => {
  const outcome = reduceTerminalFrame({ explained: false }, { type: 'error' });
  assert.deepEqual(bannerForErrorFrame(outcome.render, 'INVALID_JSON'), { kind: 'message' });
  assert.deepEqual(bannerForErrorFrame(outcome.render, 'EMPTY_CONTENT'), { kind: 'message' });
});

test('an unknown or absent error code raises no banner', () => {
  const outcome = reduceTerminalFrame({ explained: false }, { type: 'error' });
  assert.deepEqual(bannerForErrorFrame(outcome.render, 'SOMETHING_NEW'), { kind: 'none' });
  assert.deepEqual(bannerForErrorFrame(outcome.render, undefined), { kind: 'none' });
});

test('a notice frame with no text leaves the banner intact', () => {
  // Defensive pairing with the bubble fallback: a malformed notice must not
  // suppress the banner, or a real failure goes fully silent.
  const { rendered } = runFrames([
    { type: 'turn_start' },
    { type: 'context_exhausted' },
    { type: 'error' },
  ]);
  assert.deepEqual(rendered, [{ kind: 'error' }]);

  const outcome = reduceTerminalFrame({ explained: false }, { type: 'error' });
  assert.deepEqual(bannerForErrorFrame(outcome.render, 'PROVIDER_ERROR'), {
    kind: 'configuration',
  });
});

test('a new turn and a valid context notice clear a stale prior banner', () => {
  assert.equal(
    reduceTerminalFrame({ explained: false }, { type: 'turn_start' }).clearBanner,
    true,
  );
  assert.equal(
    reduceTerminalFrame(
      { explained: false },
      { type: 'context_exhausted', notice: NOTICE },
    ).clearBanner,
    true,
  );
  assert.equal(
    reduceTerminalFrame({ explained: false }, { type: 'context_exhausted' }).clearBanner,
    false,
  );
});

test('context notice presentation survives reload only when server persistence is absent', () => {
  assert.deepEqual(contextExhaustedBubblePresentation(true), {
    markdown: true,
    ephemeral: true,
  });
  assert.deepEqual(contextExhaustedBubblePresentation(false), {
    markdown: true,
    ephemeral: false,
  });
});
