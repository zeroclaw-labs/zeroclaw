/**
 * Gapless streaming audio player for TTS chunks.
 *
 * The gateway streams sentence-sized audio chunks over the chat WebSocket,
 * tagged with a monotonically increasing `seq`. Chunks are decoded
 * concurrently but SCHEDULED strictly in seq order, so sentences can never
 * play out of order even when a later, shorter chunk decodes first.
 * Playback starts the moment the first sentence is ready and never gaps
 * between sentences.
 *
 * Two wire formats are accepted per chunk:
 *  - container formats (mp3/wav/opus/...) — decoded via decodeAudioData.
 *  - "pcm_16000" — raw little-endian mono 16-bit PCM at 16 kHz with no
 *    container; decoded by hand into an AudioBuffer since decodeAudioData
 *    can't parse headerless PCM.
 *
 * An AnalyserNode taps the output so the mascot can move its body to the
 * actual audio envelope (see ClawdAvatar.setTalkLevel).
 */

const PCM16_SAMPLE_RATE = 16000;

import { RobotVoice, type VoiceEffectPreset } from './robotVoice';

export class StreamingAudioPlayer {
  private ctx: AudioContext | null = null;
  private analyser: AnalyserNode | null = null;
  private gain: GainNode | null = null;
  private nextStartTime = 0;
  private activeSources = new Set<AudioBufferSourceNode>();
  private pendingDecodes = 0;
  private generation = 0;
  private levelData: Uint8Array<ArrayBuffer> | null = null;
  private robot: RobotVoice | null = null;
  private endedTimer: ReturnType<typeof setTimeout> | null = null;

  // seq-ordered scheduling: decoded buffers wait here until their turn.
  private decoded = new Map<number, AudioBuffer>();
  private nextSeq = 0;
  private fallbackSeq = 0;

  // Anti-click barge-in: sources stopping are held here until the gain
  // ramp finishes, then hard-stopped and the master gain is restored.
  private stopPending: AudioBufferSourceNode[] = [];
  private gainRestoreTimer: ReturnType<typeof setTimeout> | null = null;

  /** Fired when the queue fully drains after at least one chunk played. */
  public onPlaybackEnd: (() => void) | null = null;
  /** Fired when the first chunk of an utterance actually starts playing. */
  public onPlaybackStart: (() => void) | null = null;
  /** Fired with a chunk's `seq` the moment its source actually starts
   *  playing (not on receipt/decode) — drives mascot_cue timing. */
  /**
   * Fired when the FIRST audio chunk of a sentence unit starts playing,
   * with that unit's index — drives audio-locked mascot cues. In the HTTP
   * TTS path every chunk is its own unit; in the streaming path several
   * frames may share one unit (`unit_seq` on the wire).
   */
  public onUnitStart: ((unitSeq: number) => void) | null = null;
  private unitOf = new Map<number, number>();
  private lastUnitStarted = -1;

  private started = false;

  private ensureCtx(): AudioContext {
    if (!this.ctx) {
      this.ctx = new AudioContext();
      this.gain = this.ctx.createGain();
      this.analyser = this.ctx.createAnalyser();
      this.analyser.fftSize = 256;
      this.analyser.smoothingTimeConstant = 0.6;
      // gain → robot-voice FX → analyser, so both the speakers and the
      // mascot's talk envelope hear the processed character.
      this.robot = new RobotVoice(this.ctx, this.gain, this.analyser);
      this.analyser.connect(this.ctx.destination);
      this.levelData = new Uint8Array(this.analyser.frequencyBinCount);
    }
    if (this.ctx.state === 'suspended') void this.ctx.resume();
    return this.ctx;
  }

  /** Switch the voice character (droid / vox / core / human) live. */
  setVoiceEffect(preset: VoiceEffectPreset): void {
    this.ensureCtx();
    this.robot?.setPreset(preset);
  }

  /** Must be called from a user gesture at least once to unlock audio. */
  unlock(): void {
    this.ensureCtx();
  }

  /** Decode a headerless raw PCM chunk (base64 of little-endian int16 mono
   *  samples at 16 kHz) into a playable AudioBuffer. */
  private decodePcm16(ctx: AudioContext, audioB64: string): AudioBuffer {
    const raw = atob(audioB64);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    const n = bytes.length >> 1;
    const buffer = ctx.createBuffer(1, n, PCM16_SAMPLE_RATE);
    const channel = buffer.getChannelData(0);
    const view = new DataView(bytes.buffer);
    for (let i = 0; i < n; i++) {
      channel[i] = view.getInt16(i * 2, true) / 32768;
    }
    return buffer;
  }

  /**
   * Queue a base64 audio chunk. `seq` is the server's sentence index for
   * this utterance; chunks play in seq order regardless of decode timing.
   * Omitting seq falls back to arrival order. `format` selects the decode
   * path — "pcm_16000" for raw PCM, anything else (or omitted) uses the
   * browser's container decoder.
   */
  async enqueue(
    audioB64: string,
    seq?: number,
    format?: string,
    unitSeq?: number,
  ): Promise<void> {
    const ctx = this.ensureCtx();
    // Defensive: if a prior cancel()'s stop+restore timer never got a
    // chance to run (e.g. torn down mid-ramp), don't let the next
    // utterance start with the master gain still ducked from barge-in.
    if (this.stopPending.length > 0 || this.gainRestoreTimer) this.flushStopPending();
    const gen = this.generation;
    const mySeq = seq ?? this.fallbackSeq;
    this.fallbackSeq = Math.max(this.fallbackSeq, mySeq) + 1;
    this.unitOf.set(mySeq, unitSeq ?? mySeq);
    this.pendingDecodes++;
    try {
      const buffer =
        format === 'pcm_16000'
          ? this.decodePcm16(ctx, audioB64)
          : await (async () => {
              const raw = atob(audioB64);
              const bytes = new Uint8Array(raw.length);
              for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
              return ctx.decodeAudioData(bytes.buffer);
            })();
      if (gen !== this.generation) return; // cancelled while decoding
      this.decoded.set(mySeq, buffer);
      this.drainReady(ctx);
    } catch {
      // Undecodable chunk — skip it rather than stalling the queue.
      if (gen === this.generation && mySeq === this.nextSeq) this.nextSeq++;
      if (gen === this.generation) this.drainReady(ctx);
    } finally {
      if (gen === this.generation) {
        this.pendingDecodes--;
        this.maybeFinished();
      }
    }
  }

  /** Schedule every consecutively-ready buffer starting at nextSeq. */
  private drainReady(ctx: AudioContext): void {
    let buffer = this.decoded.get(this.nextSeq);
    while (buffer) {
      const seqForThis = this.nextSeq;
      this.decoded.delete(this.nextSeq);
      this.nextSeq++;
      const src = ctx.createBufferSource();
      src.buffer = buffer;
      src.connect(this.gain!);
      const startAt = Math.max(ctx.currentTime + 0.02, this.nextStartTime);
      src.start(startAt);
      this.nextStartTime = startAt + buffer.duration;
      this.activeSources.add(src);
      if (!this.started) {
        this.started = true;
        this.onPlaybackStart?.();
      }
      const gen = this.generation;
      const unitForThis = this.unitOf.get(seqForThis) ?? seqForThis;
      this.unitOf.delete(seqForThis);
      const delayMs = Math.max(0, (startAt - ctx.currentTime) * 1000);
      setTimeout(() => {
        if (gen !== this.generation) return;
        if (unitForThis > this.lastUnitStarted) {
          this.lastUnitStarted = unitForThis;
          this.onUnitStart?.(unitForThis);
        }
      }, delayMs);
      src.onended = () => {
        this.activeSources.delete(src);
        this.maybeFinished();
      };
      buffer = this.decoded.get(this.nextSeq);
    }
  }

  private maybeFinished(): void {
    if (this.endedTimer) {
      clearTimeout(this.endedTimer);
      this.endedTimer = null;
    }
    if (
      this.started &&
      this.activeSources.size === 0 &&
      this.pendingDecodes === 0 &&
      this.decoded.size === 0
    ) {
      // Debounce: another sentence chunk may be right behind on the socket.
      this.endedTimer = setTimeout(() => {
        if (
          this.started &&
          this.activeSources.size === 0 &&
          this.pendingDecodes === 0 &&
          this.decoded.size === 0
        ) {
          this.started = false;
          this.nextStartTime = 0;
          this.onPlaybackEnd?.();
        }
      }, 350);
    }
  }

  /** True while audio is audibly playing or queued. */
  get playing(): boolean {
    return this.started;
  }

  /** Instantaneous output level 0..1 — drives the mascot's talking motion. */
  level(): number {
    if (!this.analyser || !this.levelData || !this.started) return 0;
    this.analyser.getByteFrequencyData(this.levelData);
    let sum = 0;
    // Voice energy lives in the lower bins; weight them.
    const n = Math.min(48, this.levelData.length);
    for (let i = 0; i < n; i++) sum += this.levelData[i] ?? 0;
    return Math.min(1, sum / n / 160);
  }

  /** Hard-stop every source in `stopPending`, clear it, and restore the
   *  master gain to full — the tail end of a barge-in's anti-click ramp,
   *  also usable as a defensive flush before a fresh utterance starts. */
  private flushStopPending(): void {
    if (this.gainRestoreTimer) {
      clearTimeout(this.gainRestoreTimer);
      this.gainRestoreTimer = null;
    }
    for (const src of this.stopPending) {
      try {
        src.onended = null;
        src.stop();
      } catch {
        // already stopped
      }
    }
    this.stopPending = [];
    this.restoreGain();
  }

  private restoreGain(): void {
    if (!this.gain || !this.ctx) return;
    const now = this.ctx.currentTime;
    this.gain.gain.cancelScheduledValues(now);
    this.gain.gain.setValueAtTime(1, now);
  }

  /**
   * Stop everything immediately (barge-in) and reset seq tracking for the
   * next utterance. Chunks still decoding from the old generation are
   * discarded when they complete.
   *
   * To avoid an audible click, the master gain is ramped down first and
   * the actual source.stop() calls happen ~10ms later, once the ramp has
   * silenced them; the gain is then restored to 1 for the next utterance.
   */
  cancel(): void {
    this.generation++;
    this.pendingDecodes = 0;
    this.decoded.clear();
    this.nextSeq = 0;
    this.fallbackSeq = 0;
    this.unitOf.clear();
    this.lastUnitStarted = -1;

    const ctx = this.ctx;
    const gain = this.gain;
    for (const src of this.activeSources) this.stopPending.push(src);
    this.activeSources.clear();

    if (ctx && gain) {
      const now = ctx.currentTime;
      gain.gain.cancelScheduledValues(now);
      gain.gain.setValueAtTime(Math.max(gain.gain.value, 0.0001), now);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.008);
      if (this.gainRestoreTimer) clearTimeout(this.gainRestoreTimer);
      this.gainRestoreTimer = setTimeout(() => {
        this.gainRestoreTimer = null;
        this.flushStopPending();
      }, 10);
    } else {
      this.flushStopPending();
    }

    this.nextStartTime = 0;
    if (this.endedTimer) {
      clearTimeout(this.endedTimer);
      this.endedTimer = null;
    }
    if (this.started) {
      this.started = false;
      this.onPlaybackEnd?.();
    }
  }

  /**
   * Soft-duck playback to a fraction of full volume in ~30ms — used the
   * instant the VAD sees a possible speech onset, so the companion
   * audibly yields before the interruption is even confirmed.
   */
  duck(to = 0.2): void {
    const ctx = this.ctx;
    const gain = this.gain;
    if (!ctx || !gain) return;
    const now = ctx.currentTime;
    gain.gain.cancelScheduledValues(now);
    gain.gain.setValueAtTime(Math.max(gain.gain.value, 0.0001), now);
    gain.gain.exponentialRampToValueAtTime(Math.max(to, 0.0001), now + 0.03);
  }

  /** Undo duck() — the candidate onset turned out to be noise. */
  unduck(): void {
    const ctx = this.ctx;
    const gain = this.gain;
    if (!ctx || !gain) return;
    const now = ctx.currentTime;
    gain.gain.cancelScheduledValues(now);
    gain.gain.setValueAtTime(Math.max(gain.gain.value, 0.0001), now);
    gain.gain.exponentialRampToValueAtTime(1, now + 0.08);
  }

  /** Reset seq tracking at the start of a new utterance/turn. */
  beginUtterance(): void {
    this.nextSeq = 0;
    this.fallbackSeq = 0;
    this.decoded.clear();
    this.unitOf.clear();
    this.lastUnitStarted = -1;
  }

  dispose(): void {
    this.cancel();
    this.robot?.dispose();
    this.robot = null;
    void this.ctx?.close();
    this.ctx = null;
    this.analyser = null;
    this.gain = null;
  }
}
