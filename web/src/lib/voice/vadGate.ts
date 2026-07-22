/**
 * VadGate — the pure decision core of the playback-aware voice-activity
 * detector. Extracted from MicCapture so the thresholds are unit-testable:
 * a mis-calibrated formula here silently kills barge-in (the bug this
 * refactor fixed: a threshold expressed in a normalized 0..1 UI scale is
 * unreachable in raw mic-RMS units, where normal speech measures
 * ~0.02–0.15).
 *
 * All levels are raw RMS of Float32 PCM frames (0..~0.5 in practice).
 * `playbackLevel` is the TTS output envelope (0..1, a UI-scale value fed
 * from the player's analyser) — it only nudges the bar, it must never
 * dominate it, because `echoCancellation: true` already removes most of
 * the agent's own voice from the mic signal.
 */

export const PLAYBACK_ACTIVE_LEVEL = 0.015;

function clamp(v: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, v));
}

/** Ambient onset threshold derived from the noise-floor EMA. */
export function idleOnsetThreshold(noiseFloor: number): number {
  return clamp(noiseFloor * 3.1 + 0.024, 0.045, 0.14);
}

/**
 * Onset threshold while the agent is audibly speaking. Slightly above the
 * idle bar — enough to reject residual echo that AEC missed, low enough
 * that ordinary speech (RMS ≳ 0.05) still crosses it easily.
 */
export function playbackOnsetThreshold(noiseFloor: number, playbackLevel: number): number {
  const idle = idleOnsetThreshold(noiseFloor);
  return clamp(Math.max(idle * 1.35, idle + 0.012 + playbackLevel * 0.02), 0.045, 0.09);
}

/** How long the signal must stay over the bar before onset is confirmed. */
export function sustainMsFor(duringPlayback: boolean): number {
  return duringPlayback ? 170 : 130;
}

export type VadEvent =
  | 'none'
  /** first over-threshold frame — a candidate onset (soft-duck now) */
  | 'possible'
  /** sustained over-threshold — confirmed speech onset */
  | 'onset'
  /** candidate collapsed back below the bar (undo the soft-duck) */
  | 'reset';

export class VadGate {
  private noiseFloorValue = 0.02;
  private overMs = 0;
  private candidate = false;

  get noiseFloor(): number {
    return this.noiseFloorValue;
  }

  idleThreshold(): number {
    return idleOnsetThreshold(this.noiseFloorValue);
  }

  /** Feed one frame; returns the gate's decision for this frame. */
  observe(rms: number, frameMs: number, playbackLevel: number): VadEvent {
    const duringPlayback = playbackLevel > PLAYBACK_ACTIVE_LEVEL;
    const idle = this.idleThreshold();
    const threshold = duringPlayback
      ? playbackOnsetThreshold(this.noiseFloorValue, playbackLevel)
      : idle;

    // Adapt the floor only while quiet and the agent isn't audibly talking,
    // so speech and TTS bleed never pollute the ambient estimate.
    if (rms < idle && !duringPlayback) {
      this.noiseFloorValue = this.noiseFloorValue * 0.985 + rms * 0.015;
    }

    if (rms >= threshold) {
      const wasCandidate = this.candidate;
      this.candidate = true;
      this.overMs += frameMs;
      if (this.overMs >= sustainMsFor(duringPlayback)) {
        this.candidate = false;
        this.overMs = 0;
        return 'onset';
      }
      return wasCandidate ? 'none' : 'possible';
    }

    this.overMs = 0;
    if (this.candidate) {
      this.candidate = false;
      return 'reset';
    }
    return 'none';
  }

  resetCandidate(): void {
    this.candidate = false;
    this.overMs = 0;
  }
}
