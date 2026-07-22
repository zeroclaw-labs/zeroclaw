/**
 * robotVoice — synthetic voice characters for the companion.
 *
 * A WebAudio effect chain inserted between the TTS player's master gain
 * and its analyser, so the mascot's talk animation still tracks the
 * processed sound. Ring modulation is the backbone (the classic robot
 * timbre), shaped differently per preset; a waveshaper adds grit where the
 * character calls for it.
 *
 * Pure client-side DSP: works identically over the streaming PCM path and
 * the HTTP mp3 path, adds zero latency worth measuring, and the underlying
 * ElevenLabs voice still carries the prosody — so the robot sounds alive,
 * not flat.
 */

export type VoiceEffectPreset = 'droid' | 'vox' | 'core' | 'human';

export const VOICE_EFFECT_PRESETS: Array<{
  id: VoiceEffectPreset;
  name: string;
  blurb: string;
}> = [
  { id: 'droid', name: 'Droid', blurb: 'warm robot — gentle metallic shimmer' },
  { id: 'vox', name: 'Vox', blurb: 'classic sci-fi vocoder timbre' },
  { id: 'core', name: 'Core', blurb: 'deep synthetic, slightly gritty' },
  { id: 'human', name: 'Human', blurb: 'the raw ElevenLabs voice' },
];

const STORAGE_KEY = 'zeroclaw_voice_effect';

/** The companion defaults to a robot character; humans are opt-in. */
export function storedVoiceEffect(): VoiceEffectPreset {
  const v = localStorage.getItem(STORAGE_KEY);
  if (v === 'droid' || v === 'vox' || v === 'core' || v === 'human') return v;
  return 'droid';
}

export function storeVoiceEffect(preset: VoiceEffectPreset): void {
  localStorage.setItem(STORAGE_KEY, preset);
}

interface PresetParams {
  /** ring-mod carrier frequency (Hz) */
  carrierHz: number;
  /** wet mix 0..1 (dry = 1 - wet) */
  wet: number;
  /** waveshaper drive; 0 disables */
  drive: number;
  /** optional bandpass center (Hz); 0 disables */
  bandHz: number;
  bandQ: number;
}

const PARAMS: Record<Exclude<VoiceEffectPreset, 'human'>, PresetParams> = {
  droid: { carrierHz: 32, wet: 0.45, drive: 0, bandHz: 0, bandQ: 1 },
  vox: { carrierHz: 55, wet: 0.65, drive: 0.15, bandHz: 1700, bandQ: 0.6 },
  core: { carrierHz: 19, wet: 0.55, drive: 0.3, bandHz: 900, bandQ: 0.8 },
};

function driveCurve(amount: number): Float32Array<ArrayBuffer> {
  const n = 1024;
  const curve = new Float32Array(n);
  const k = amount * 40 + 1;
  for (let i = 0; i < n; i++) {
    const x = (i / (n - 1)) * 2 - 1;
    curve[i] = Math.tanh(k * x) / Math.tanh(k);
  }
  return curve;
}

/**
 * The effect chain. Always wired input→output; `setPreset` rebalances the
 * wet/dry mix live (switching to 'human' just mutes the wet path, so a
 * change mid-sentence is glitch-free).
 */
export class RobotVoice {
  private ctx: AudioContext;
  private dry: GainNode;
  private wet: GainNode;
  private ring: GainNode;
  private carrier: OscillatorNode;
  private carrierDepth: GainNode;
  private shaper: WaveShaperNode;
  private band: BiquadFilterNode;

  constructor(ctx: AudioContext, input: AudioNode, output: AudioNode) {
    this.ctx = ctx;

    this.dry = ctx.createGain();
    this.wet = ctx.createGain();
    input.connect(this.dry);
    this.dry.connect(output);

    // wet path: input × carrier → shaper → bandpass → wet gain → output
    this.ring = ctx.createGain();
    this.ring.gain.value = 0;
    this.carrier = ctx.createOscillator();
    this.carrier.type = 'sine';
    this.carrierDepth = ctx.createGain();
    this.carrier.connect(this.carrierDepth);
    this.carrierDepth.connect(this.ring.gain);
    this.carrier.start();

    this.shaper = ctx.createWaveShaper();
    this.band = ctx.createBiquadFilter();
    this.band.type = 'bandpass';

    input.connect(this.ring);
    this.ring.connect(this.shaper);
    this.shaper.connect(this.band);
    this.band.connect(this.wet);
    this.wet.connect(output);

    this.setPreset(storedVoiceEffect());
  }

  setPreset(preset: VoiceEffectPreset): void {
    const now = this.ctx.currentTime;
    if (preset === 'human') {
      this.dry.gain.setTargetAtTime(1, now, 0.02);
      this.wet.gain.setTargetAtTime(0, now, 0.02);
      return;
    }
    const p = PARAMS[preset];
    this.carrier.frequency.setTargetAtTime(p.carrierHz, now, 0.02);
    // ring.gain oscillates ±depth around depth/…: bias at 0.5 so the
    // carrier fully modulates the signal without inverting the envelope.
    this.carrierDepth.gain.setTargetAtTime(0.5, now, 0.02);
    this.ring.gain.setTargetAtTime(0.5, now, 0.02);
    this.shaper.curve = p.drive > 0 ? driveCurve(p.drive) : null;
    if (p.bandHz > 0) {
      this.band.type = 'bandpass';
      this.band.frequency.setTargetAtTime(p.bandHz, now, 0.02);
      this.band.Q.setTargetAtTime(p.bandQ, now, 0.02);
    } else {
      this.band.type = 'allpass';
      this.band.frequency.setTargetAtTime(1000, now, 0.02);
    }
    this.dry.gain.setTargetAtTime(1 - p.wet, now, 0.02);
    this.wet.gain.setTargetAtTime(p.wet, now, 0.02);
  }

  dispose(): void {
    try {
      this.carrier.stop();
    } catch {
      // already stopped
    }
    for (const n of [this.dry, this.wet, this.ring, this.carrierDepth, this.shaper, this.band]) {
      n.disconnect();
    }
  }
}
