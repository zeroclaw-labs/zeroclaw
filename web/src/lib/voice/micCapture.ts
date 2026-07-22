/**
 * Microphone capture with voice-activity detection.
 *
 * Raw PCM is captured into a ring buffer so utterances include ~400ms of
 * pre-roll (no clipped first syllables). Two modes:
 *
 *  - push-to-talk: start()/stop() bracket the utterance explicitly
 *  - continuous: an energy VAD segments speech automatically (adaptive
 *    threshold + hangover), emitting one WAV blob per utterance
 *
 * Output is 16 kHz mono 16-bit WAV — small, universally accepted by
 * Whisper-family STT endpoints, and needs no client-side codec.
 *
 * Onset detection is playback-aware and adaptive rather than a pair of
 * fixed thresholds:
 *  - an EMA tracks the ambient noise floor while the room is quiet and the
 *    agent isn't talking;
 *  - the idle onset threshold is derived from that floor;
 *  - while TTS audio is audibly playing (fed in via `setPlaybackLevel`),
 *    the onset threshold rises further and onset must sustain longer,
 *    so only a real interruption (louder, sustained speech) counts —
 *    ambient room noise and the agent's own audio bleed don't retrigger it.
 */

export type MicState = 'idle' | 'armed' | 'speaking';

export interface MicCaptureOptions {
  /** ms of silence that ends an utterance in continuous mode. */
  silenceHangoverMs?: number;
  /** ms of silence at which a PREVIEW WAV is emitted for eager
   * transcription (must be < silenceHangoverMs). */
  previewAtMs?: number;
  /** ms of audio kept before the detected speech onset. */
  prerollMs?: number;
  /** Minimum utterance length to emit, ms (filters coughs/clicks). */
  minUtteranceMs?: number;
  /** Hard cap per utterance, ms. */
  maxUtteranceMs?: number;
}

import { VadGate } from './vadGate';

const TARGET_RATE = 16000;

function encodeWav(samples: Float32Array, sampleRate: number): Blob {
  // Downsample to 16 kHz mono.
  const ratio = sampleRate / TARGET_RATE;
  const outLen = Math.floor(samples.length / ratio);
  const pcm = new Int16Array(outLen);
  for (let i = 0; i < outLen; i++) {
    // simple average-pool downsample
    const start = Math.floor(i * ratio);
    const end = Math.min(samples.length, Math.floor((i + 1) * ratio));
    let sum = 0;
    for (let j = start; j < end; j++) sum += samples[j] ?? 0;
    const v = end > start ? sum / (end - start) : 0;
    pcm[i] = Math.max(-32768, Math.min(32767, Math.round(v * 32767)));
  }
  const dataSize = pcm.length * 2;
  const buf = new ArrayBuffer(44 + dataSize);
  const view = new DataView(buf);
  const writeStr = (off: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(off + i, s.charCodeAt(i));
  };
  writeStr(0, 'RIFF');
  view.setUint32(4, 36 + dataSize, true);
  writeStr(8, 'WAVE');
  writeStr(12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, TARGET_RATE, true);
  view.setUint32(28, TARGET_RATE * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeStr(36, 'data');
  view.setUint32(40, dataSize, true);
  new Int16Array(buf, 44).set(pcm);
  return new Blob([buf], { type: 'audio/wav' });
}

export class MicCapture {
  private stream: MediaStream | null = null;
  private ctx: AudioContext | null = null;
  private processor: ScriptProcessorNode | null = null;
  private source: MediaStreamAudioSourceNode | null = null;

  private ring: Float32Array[] = [];
  private ringSamples = 0;
  private maxRingSamples = 0;

  private capturing = false;
  private captured: Float32Array[] = [];
  private capturedSamples = 0;

  private continuous = false;
  private silenceMs = 0;
  private utteranceMs = 0;

  private opts: Required<MicCaptureOptions>;
  private currentLevel = 0;

  /** Pure playback-aware onset detector (see vadGate.ts). */
  private gate = new VadGate();
  /** Instantaneous TTS output level, pushed in by the caller each frame
   * (see `setPlaybackLevel`). Raises the onset bar while the agent talks. */
  private playbackLevel = 0;

  /** Monotonic id of the current/most recent utterance. */
  private utteranceId = 0;
  private previewFired = false;
  private previewInvalidated = false;

  /** Emitted with a finished utterance WAV. `previewValid` is true when an
   * earlier onUtterancePreview for `utteranceId` still matches this final
   * audio (no speech resumed after the preview) — its transcript can be
   * used as-is. */
  public onUtterance:
    | ((
        wav: Blob,
        durationMs: number,
        info: { utteranceId: number; previewValid: boolean },
      ) => void)
    | null = null;
  /** Early utterance snapshot for eager transcription (continuous mode). */
  public onUtterancePreview: ((wav: Blob, utteranceId: number) => void) | null = null;
  /** Speech onset detected in continuous mode. The caller decides whether
   * this counts as a fresh utterance or a barge-in based on its own turn
   * state — the mic has no notion of "turn". */
  public onSpeechStart: (() => void) | null = null;
  /** First over-threshold frame in continuous mode — speech MIGHT be
   * starting. The caller can soft-duck playback instantly so the companion
   * audibly yields the moment the user opens their mouth. */
  public onPossibleSpeech: (() => void) | null = null;
  /** The possible-speech candidate collapsed (a click, a cough, a truck) —
   * undo the soft-duck. */
  public onSpeechReset: (() => void) | null = null;

  constructor(options: MicCaptureOptions = {}) {
    this.opts = {
      silenceHangoverMs: options.silenceHangoverMs ?? 550,
      previewAtMs: options.previewAtMs ?? 300,
      prerollMs: options.prerollMs ?? 400,
      minUtteranceMs: options.minUtteranceMs ?? 250,
      maxUtteranceMs: options.maxUtteranceMs ?? 45000,
    };
  }

  /** Feed the current TTS output envelope (0..1) each frame so onset
   * detection can raise its bar while the agent is audibly speaking. */
  setPlaybackLevel(level: number): void {
    this.playbackLevel = level;
  }

  /** Adaptive idle onset threshold, derived from the tracked noise floor. */
  private idleThreshold(): number {
    return this.gate.idleThreshold();
  }

  get level(): number {
    return this.currentLevel;
  }

  get running(): boolean {
    return this.ctx !== null;
  }

  private disposed = false;

  async init(): Promise<void> {
    if (this.ctx || this.disposed) return;
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });
    // dispose() may have been called while the permission prompt was open —
    // release the just-granted stream instead of leaking a hot mic.
    if (this.disposed) {
      stream.getTracks().forEach((t) => t.stop());
      return;
    }
    this.stream = stream;
    this.ctx = new AudioContext();
    this.source = this.ctx.createMediaStreamSource(this.stream);
    // ScriptProcessor is deprecated but universally supported; 2048
    // samples ≈ 43 ms at 48 kHz — fine enough granularity for snappy
    // onset/barge-in decisions. An AudioWorklet swap is a contained
    // follow-up.
    this.processor = this.ctx.createScriptProcessor(2048, 1, 1);
    this.maxRingSamples = Math.ceil((this.opts.prerollMs / 1000) * this.ctx.sampleRate);
    this.processor.onaudioprocess = (ev) => this.handleFrame(ev.inputBuffer.getChannelData(0));
    this.source.connect(this.processor);
    // Keep the node alive without echoing mic to speakers.
    const sink = this.ctx.createGain();
    sink.gain.value = 0;
    this.processor.connect(sink);
    sink.connect(this.ctx.destination);
  }

  private handleFrame(input: Float32Array): void {
    const frame = new Float32Array(input); // copy — buffer is reused
    const frameMs = (frame.length / (this.ctx?.sampleRate ?? 48000)) * 1000;

    let sum = 0;
    for (let i = 0; i < frame.length; i++) {
      const s = frame[i] ?? 0;
      sum += s * s;
    }
    const rms = Math.sqrt(sum / frame.length);
    this.currentLevel = rms;

    // preroll ring
    this.ring.push(frame);
    this.ringSamples += frame.length;
    while (this.ringSamples - (this.ring[0]?.length ?? 0) > this.maxRingSamples) {
      this.ringSamples -= this.ring.shift()!.length;
    }

    if (this.capturing) {
      this.captured.push(frame);
      this.capturedSamples += frame.length;
      this.utteranceMs += frameMs;

      if (this.continuous) {
        // Mid-utterance silence detection uses the same adaptive idle
        // threshold as onset — the floor isn't updated here (the frame may
        // well still be speech), only read.
        if (rms >= this.idleThreshold()) {
          this.silenceMs = 0;
          // Speech resumed after a preview fired — that transcript is
          // stale; the final utterance will differ.
          if (this.previewFired) this.previewInvalidated = true;
        } else {
          this.silenceMs += frameMs;
        }
        // Eager transcription: once silence PROBABLY means the utterance is
        // over (but before the full hangover confirms it), hand the caller
        // a preview WAV so STT can run concurrently with the remaining
        // hangover. If the silence holds, the transcript is ready the
        // instant the utterance commits — the STT wait vanishes from the
        // perceived response time.
        if (
          !this.previewFired &&
          this.silenceMs >= this.opts.previewAtMs &&
          this.utteranceMs - this.silenceMs >= this.opts.minUtteranceMs
        ) {
          this.previewFired = true;
          this.previewInvalidated = false;
          this.onUtterancePreview?.(this.assembleWav(), this.utteranceId);
        }
        if (this.silenceMs >= this.opts.silenceHangoverMs || this.utteranceMs >= this.opts.maxUtteranceMs) {
          this.finishUtterance();
        }
      } else if (this.utteranceMs >= this.opts.maxUtteranceMs) {
        this.finishUtterance();
      }
      return;
    }

    // Not capturing: continuous-mode onset detection via the pure gate.
    if (!this.continuous) return;
    switch (this.gate.observe(rms, frameMs, this.playbackLevel)) {
      case 'possible':
        this.onPossibleSpeech?.();
        break;
      case 'onset':
        this.onSpeechStart?.();
        this.beginCapture(true);
        break;
      case 'reset':
        this.onSpeechReset?.();
        break;
      default:
        break;
    }
  }

  /** Encode the samples captured so far without ending the utterance. */
  private assembleWav(): Blob {
    const all = new Float32Array(this.captured.reduce((n, c) => n + c.length, 0));
    let off = 0;
    for (const c of this.captured) {
      all.set(c, off);
      off += c.length;
    }
    return encodeWav(all, this.ctx?.sampleRate ?? 48000);
  }

  private beginCapture(includePreroll: boolean): void {
    this.utteranceId += 1;
    this.previewFired = false;
    this.previewInvalidated = false;
    this.capturing = true;
    this.captured = includePreroll ? [...this.ring] : [];
    this.capturedSamples = includePreroll ? this.ringSamples : 0;
    this.silenceMs = 0;
    this.utteranceMs = 0;
    this.gate.resetCandidate();
  }

  private finishUtterance(): void {
    if (!this.capturing) return;
    this.capturing = false;
    const durationMs = (this.capturedSamples / (this.ctx?.sampleRate ?? 48000)) * 1000;
    const chunks = this.captured;
    this.captured = [];
    this.capturedSamples = 0;
    // Reject blips on ACTUAL SPEECH TIME. In continuous mode the trailing
    // silence hangover is part of the elapsed capture, so a raw duration
    // check could never reject anything — a cough would ride 550ms of
    // silence past any threshold, chime, and burn an STT call. utteranceMs
    // counts post-onset frames only (preroll excluded) and silenceMs is the
    // trailing quiet, so their difference is the voiced span.
    const speechMs = this.continuous
      ? this.utteranceMs - this.silenceMs
      : durationMs - this.opts.prerollMs;
    if (speechMs < this.opts.minUtteranceMs) return;
    const all = new Float32Array(chunks.reduce((n, c) => n + c.length, 0));
    let off = 0;
    for (const c of chunks) {
      all.set(c, off);
      off += c.length;
    }
    const wav = encodeWav(all, this.ctx?.sampleRate ?? 48000);
    this.onUtterance?.(wav, durationMs, {
      utteranceId: this.utteranceId,
      previewValid: this.previewFired && !this.previewInvalidated,
    });
  }

  /** Push-to-talk: begin capturing now (includes preroll). */
  startPushToTalk(): void {
    if (!this.ctx || this.capturing) return;
    this.continuous = false;
    this.beginCapture(true);
  }

  /** Push-to-talk: finish and emit the utterance. */
  stopPushToTalk(): void {
    if (this.continuous) return;
    this.finishUtterance();
  }

  /** Enable/disable continuous VAD segmentation. */
  setContinuous(on: boolean): void {
    this.continuous = on;
    if (!on && this.capturing) this.finishUtterance();
    this.gate.resetCandidate();
    this.silenceMs = 0;
  }

  dispose(): void {
    this.disposed = true;
    this.processor?.disconnect();
    this.source?.disconnect();
    this.stream?.getTracks().forEach((t) => t.stop());
    void this.ctx?.close();
    this.processor = null;
    this.source = null;
    this.stream = null;
    this.ctx = null;
    this.capturing = false;
  }
}
