/**
 * VadGate calibration tests — run with: npx jiti src/lib/voice/vadGate.logic.test.ts
 *
 * These exist because the barge-in threshold once mixed two level scales
 * (a normalized 0..1 UI scale vs raw mic RMS) and became unreachable —
 * continuous mode silently lost the ability to interrupt. The numbers
 * here pin the calibration to realistic mic RMS values.
 */
import assert from 'node:assert';
import {
  VadGate,
  idleOnsetThreshold,
  playbackOnsetThreshold,
} from './vadGate';

const FRAME_MS = 43; // 2048 samples at 48kHz

// ── threshold calibration ────────────────────────────────────────────

// Quiet room: floor ~0.008 → idle bar well below normal speech.
{
  const idle = idleOnsetThreshold(0.008);
  assert.ok(idle >= 0.045 && idle <= 0.06, `idle bar sane: ${idle}`);
}

// CRITICAL: while the agent speaks at FULL volume, ordinary speech
// (RMS ≈ 0.06–0.12) must still cross the barge-in bar.
{
  for (const playback of [0.2, 0.5, 0.8, 1.0]) {
    const bar = playbackOnsetThreshold(0.008, playback);
    assert.ok(
      bar <= 0.09,
      `barge-in bar reachable at playback=${playback}: ${bar}`,
    );
    assert.ok(
      bar > idleOnsetThreshold(0.008),
      `playback bar above idle bar at playback=${playback}`,
    );
  }
}

// Loud room: floor 0.04 → idle bar rises but stays under the clamp.
{
  const idle = idleOnsetThreshold(0.04);
  assert.ok(idle <= 0.149, `noisy-room idle bar clamped: ${idle}`);
}

// ── gate behavior ────────────────────────────────────────────────────

function feed(gate: VadGate, rms: number, frames: number, playback = 0) {
  const events: string[] = [];
  for (let i = 0; i < frames; i++) events.push(gate.observe(rms, FRAME_MS, playback));
  return events;
}

// Sustained speech while the agent talks → possible, then onset.
{
  const gate = new VadGate();
  feed(gate, 0.005, 40); // settle the floor in a quiet room
  const events = feed(gate, 0.09, 6, 0.7);
  assert.strictEqual(events[0], 'possible', 'first loud frame is a candidate');
  assert.ok(events.includes('onset'), `speaking over TTS triggers barge-in: ${events}`);
  const confirmFrames = events.indexOf('onset') + 1;
  assert.ok(
    confirmFrames * FRAME_MS <= 260,
    `barge-in confirms fast (${confirmFrames * FRAME_MS}ms)`,
  );
}

// A single loud pop during playback → possible, then reset — never onset.
{
  const gate = new VadGate();
  feed(gate, 0.005, 40);
  const pop = [...feed(gate, 0.2, 1, 0.7), ...feed(gate, 0.005, 3, 0.7)];
  assert.strictEqual(pop[0], 'possible');
  assert.ok(pop.includes('reset'), `false alarm resets: ${pop}`);
  assert.ok(!pop.includes('onset'), 'a pop never confirms');
}

// TTS bleed after echo cancellation (residual RMS ~0.02) never triggers
// onset while the agent is talking.
{
  const gate = new VadGate();
  feed(gate, 0.005, 40);
  const bleed = feed(gate, 0.02, 60, 0.9);
  assert.ok(!bleed.includes('onset'), 'AEC residual never barges in');
}

// Quiet-room ambient noise never triggers while idle.
{
  const gate = new VadGate();
  const ambient = feed(gate, 0.01, 100, 0);
  assert.ok(!ambient.includes('onset'), 'ambient noise never onsets');
}

// Normal speech in idle mode confirms within ~180ms.
{
  const gate = new VadGate();
  feed(gate, 0.005, 40);
  const events = feed(gate, 0.07, 5, 0);
  const at = events.indexOf('onset');
  assert.ok(at >= 0 && (at + 1) * FRAME_MS <= 180, `idle onset fast: ${events}`);
}

console.log('vadGate logic tests: all passed');
