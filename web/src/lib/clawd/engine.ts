/**
 * Clawd animation engine — framework-agnostic procedural animator.
 *
 * The engine composes a Pose each frame from additive layers:
 *   base (rest pose) + idle (breathing, weight shifts) + action (named
 *   keyframe clips) + talk (audio-reactive) + emotion (persistent posture).
 *
 * All track values are deltas from the rest pose, so layers stack cleanly.
 */

export type EyeShape =
  | 'rect' // default vertical rounded slit
  | 'wide' // enlarged rect, surprise/attention
  | 'happy' // ^ ^ upward arcs
  | 'squint' // > < chevrons (effort/laughing hard)
  | 'closed' // horizontal line
  | 'sleepy' // half-lowered rect
  | 'sad' // outward-slanted droop
  | 'angry' // inward-slanted slits
  | 'heart'
  | 'star'
  | 'dizzy' // spiral-ish
  | 'o' // round o_o
  | 'x' // x_x
  | 'wink' // left closed, right open (rendered per-eye)
  | 'suspicious' // narrow horizontal band
  | 'sparkle';

export interface Pose {
  body: { x: number; y: number; rot: number; sx: number; sy: number };
  armL: { rot: number };
  armR: { rot: number };
  /** vertical offsets for the four legs, left→right */
  legs: [number, number, number, number];
  eyes: {
    shape: EyeShape;
    /** 0 = fully closed, 1 = fully open (scaleY on the shape) */
    open: number;
    /** gaze offset in rig units */
    dx: number;
    dy: number;
    size: number;
  };
  /** 0..1 — ambient energy ring used for the listening state */
  glow: number;
}

export const REST_POSE: Pose = {
  body: { x: 0, y: 0, rot: 0, sx: 1, sy: 1 },
  armL: { rot: 0 },
  armR: { rot: 0 },
  legs: [0, 0, 0, 0],
  eyes: { shape: 'rect', open: 1, dx: 0, dy: 0, size: 1 },
  glow: 0,
};

export type Channel =
  | 'body.x'
  | 'body.y'
  | 'body.rot'
  | 'body.sx'
  | 'body.sy'
  | 'armL.rot'
  | 'armR.rot'
  | 'leg0'
  | 'leg1'
  | 'leg2'
  | 'leg3'
  | 'eyes.open'
  | 'eyes.dx'
  | 'eyes.dy'
  | 'eyes.size'
  | 'glow';

export type EaseName =
  | 'linear'
  | 'inQuad'
  | 'outQuad'
  | 'inOutQuad'
  | 'inCubic'
  | 'outCubic'
  | 'inOutCubic'
  | 'outBack'
  | 'inBack'
  | 'outElastic'
  | 'outBounce'
  | 'step';

const EASE: Record<EaseName, (t: number) => number> = {
  linear: (t) => t,
  inQuad: (t) => t * t,
  outQuad: (t) => t * (2 - t),
  inOutQuad: (t) => (t < 0.5 ? 2 * t * t : -1 + (4 - 2 * t) * t),
  inCubic: (t) => t * t * t,
  outCubic: (t) => --t * t * t + 1,
  inOutCubic: (t) => (t < 0.5 ? 4 * t * t * t : (t - 1) * (2 * t - 2) * (2 * t - 2) + 1),
  outBack: (t) => {
    const c1 = 1.70158;
    const c3 = c1 + 1;
    return 1 + c3 * Math.pow(t - 1, 3) + c1 * Math.pow(t - 1, 2);
  },
  inBack: (t) => {
    const c1 = 1.70158;
    const c3 = c1 + 1;
    return c3 * t * t * t - c1 * t * t;
  },
  outElastic: (t) => {
    if (t === 0 || t === 1) return t;
    const c4 = (2 * Math.PI) / 3;
    return Math.pow(2, -10 * t) * Math.sin((t * 10 - 0.75) * c4) + 1;
  },
  outBounce: (t) => {
    const n1 = 7.5625;
    const d1 = 2.75;
    if (t < 1 / d1) return n1 * t * t;
    if (t < 2 / d1) return n1 * (t -= 1.5 / d1) * t + 0.75;
    if (t < 2.5 / d1) return n1 * (t -= 2.25 / d1) * t + 0.9375;
    return n1 * (t -= 2.625 / d1) * t + 0.984375;
  },
  step: (t) => (t < 1 ? 0 : 1),
};

/** [normalizedTime 0..1, value, easeIntoThisKey?] */
export type Keyframe = [number, number, EaseName?];

export interface ActionDef {
  /** clip length in ms */
  duration: number;
  loop?: boolean;
  /** eye shape held while the clip plays */
  eyeShape?: EyeShape;
  tracks: Partial<Record<Channel, Keyframe[]>>;
}

function sampleTrack(kfs: Keyframe[], t: number): number {
  const first = kfs[0];
  if (!first) return 0;
  if (t <= first[0]) return first[1];
  for (let i = 1; i < kfs.length; i++) {
    const cur = kfs[i]!;
    if (t <= cur[0]) {
      const [t0, v0] = kfs[i - 1]!;
      const [t1, v1, ease] = cur;
      const span = t1 - t0;
      const u = span <= 0 ? 1 : (t - t0) / span;
      return v0 + (v1 - v0) * EASE[ease ?? 'inOutQuad'](u);
    }
  }
  return kfs[kfs.length - 1]![1];
}

export type EmotionName =
  | 'neutral'
  | 'happy'
  | 'excited'
  | 'curious'
  | 'thinking'
  | 'sleepy'
  | 'sad'
  | 'love'
  | 'proud'
  | 'mischievous'
  | 'focused'
  | 'surprised';

interface EmotionDef {
  eyeShape: EyeShape;
  eyeSize: number;
  bodyRot: number;
  bodyY: number;
  breatheRate: number; // multiplier
  blinkRate: number; // multiplier on blink frequency
}

export const EMOTIONS: Record<EmotionName, EmotionDef> = {
  neutral: { eyeShape: 'rect', eyeSize: 1, bodyRot: 0, bodyY: 0, breatheRate: 1, blinkRate: 1 },
  happy: { eyeShape: 'happy', eyeSize: 1, bodyRot: 0, bodyY: -1, breatheRate: 1.15, blinkRate: 0.8 },
  excited: { eyeShape: 'wide', eyeSize: 1.15, bodyRot: 0, bodyY: -2, breatheRate: 1.5, blinkRate: 0.6 },
  curious: { eyeShape: 'rect', eyeSize: 1.08, bodyRot: 4, bodyY: 0, breatheRate: 1.1, blinkRate: 1.1 },
  thinking: { eyeShape: 'suspicious', eyeSize: 1, bodyRot: -3, bodyY: 0, breatheRate: 0.9, blinkRate: 1.3 },
  sleepy: { eyeShape: 'sleepy', eyeSize: 1, bodyRot: 0, bodyY: 2, breatheRate: 0.6, blinkRate: 2.5 },
  sad: { eyeShape: 'sad', eyeSize: 0.95, bodyRot: 0, bodyY: 3, breatheRate: 0.75, blinkRate: 1.2 },
  love: { eyeShape: 'heart', eyeSize: 1.1, bodyRot: 0, bodyY: -1, breatheRate: 1.2, blinkRate: 0.4 },
  proud: { eyeShape: 'happy', eyeSize: 1, bodyRot: 0, bodyY: -2, breatheRate: 1, blinkRate: 0.8 },
  mischievous: { eyeShape: 'suspicious', eyeSize: 1, bodyRot: 2, bodyY: 0, breatheRate: 1.1, blinkRate: 0.9 },
  focused: { eyeShape: 'rect', eyeSize: 0.92, bodyRot: 0, bodyY: 0, breatheRate: 0.85, blinkRate: 0.7 },
  surprised: { eyeShape: 'o', eyeSize: 1.2, bodyRot: 0, bodyY: -1, breatheRate: 1.4, blinkRate: 0.5 },
};

interface ActivePlayback {
  name: string;
  def: ActionDef;
  elapsed: number;
  resolve?: () => void;
}

export interface AnimatorState {
  emotion: EmotionName;
  action: string | null;
  talking: boolean;
  listening: boolean;
}

/**
 * The Animator owns all animation state. Call update(dt) each frame and
 * render the returned Pose.
 */
export class ClawdAnimator {
  private actions: Record<string, ActionDef>;
  private playback: ActivePlayback | null = null;
  private emotion: EmotionName = 'neutral';
  private time = 0;

  // blink layer
  private blinkTimer = this.nextBlinkDelay();
  private blinkPhase = -1; // <0 idle, otherwise 0..1 through the blink

  // talk layer — fed by audio levels, smoothed here
  private talkTarget = 0;
  private talkLevel = 0;
  private talking = false;

  // listening layer
  private listening = false;
  private glowLevel = 0;

  // gaze layer
  private gazeTarget = { x: 0, y: 0 };
  private gaze = { x: 0, y: 0 };
  private wanderTimer = 3;

  // micro-fidget when idle for a while
  private idleFor = 0;
  private onFidget: (() => string | null) | null = null;

  constructor(actions: Record<string, ActionDef>) {
    this.actions = actions;
  }

  /** Provide a callback that picks an occasional idle fidget action name. */
  setFidgetPicker(cb: (() => string | null) | null) {
    this.onFidget = cb;
  }

  listActions(): string[] {
    return Object.keys(this.actions);
  }

  getState(): AnimatorState {
    return {
      emotion: this.emotion,
      action: this.playback?.name ?? null,
      talking: this.talking,
      listening: this.listening,
    };
  }

  /** Play a named action. Returns a promise resolving when the clip ends (immediately for loops). */
  play(name: string): Promise<void> {
    const def = this.actions[name];
    if (!def) return Promise.resolve();
    this.playback?.resolve?.();
    if (def.loop) {
      this.playback = { name, def, elapsed: 0 };
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      this.playback = { name, def, elapsed: 0, resolve };
    });
  }

  stopAction() {
    this.playback?.resolve?.();
    this.playback = null;
  }

  setEmotion(e: EmotionName) {
    if (EMOTIONS[e]) this.emotion = e;
  }

  setTalking(on: boolean) {
    this.talking = on;
    if (!on) this.talkTarget = 0;
  }

  /** Feed instantaneous output-audio level, 0..1. */
  setTalkLevel(level: number) {
    this.talkTarget = Math.max(0, Math.min(1, level));
  }

  setListening(on: boolean) {
    this.listening = on;
  }

  /** Point the gaze somewhere, in [-1,1] screen-ish coordinates. */
  lookAt(x: number, y: number) {
    this.gazeTarget = { x: Math.max(-1, Math.min(1, x)), y: Math.max(-1, Math.min(1, y)) };
    this.wanderTimer = 2 + Math.random() * 3;
  }

  private nextBlinkDelay(): number {
    return 2 + Math.random() * 4;
  }

  /** Advance by dt seconds and produce the frame's pose. */
  update(dt: number): Pose {
    this.time += dt;
    const emo = EMOTIONS[this.emotion];

    const p: Pose = structuredClone(REST_POSE);
    p.eyes.shape = emo.eyeShape;
    p.eyes.size = emo.eyeSize;
    p.body.rot = emo.bodyRot;
    p.body.y = emo.bodyY;

    // ---- idle breathing (always on, scaled by emotion) ----
    const breathe = Math.sin(this.time * 2.2 * emo.breatheRate);
    p.body.sy += breathe * 0.012;
    p.body.sx -= breathe * 0.006;
    p.body.y += breathe * 0.6;
    p.armL.rot += breathe * 1.2;
    p.armR.rot -= breathe * 1.2;

    // ---- blink layer ----
    if (this.blinkPhase < 0) {
      this.blinkTimer -= dt * (this.playback || this.talking ? 0.6 : 1) * (1 / emo.blinkRate);
      if (this.blinkTimer <= 0) {
        this.blinkPhase = 0;
      }
    } else {
      this.blinkPhase += dt / 0.14; // 140ms blink
      if (this.blinkPhase >= 1) {
        this.blinkPhase = -1;
        this.blinkTimer = this.nextBlinkDelay();
      }
    }
    let blinkOpen = 1;
    if (this.blinkPhase >= 0) {
      // close fast, open slower
      blinkOpen = this.blinkPhase < 0.4 ? 1 - this.blinkPhase / 0.4 : (this.blinkPhase - 0.4) / 0.6;
    }

    // ---- gaze layer: ease toward target, occasionally wander ----
    this.wanderTimer -= dt;
    if (this.wanderTimer <= 0) {
      this.gazeTarget = {
        x: (Math.random() - 0.5) * 0.7,
        y: (Math.random() - 0.5) * 0.4,
      };
      this.wanderTimer = 2.5 + Math.random() * 4;
    }
    const gk = 1 - Math.exp(-dt * 8);
    this.gaze.x += (this.gazeTarget.x - this.gaze.x) * gk;
    this.gaze.y += (this.gazeTarget.y - this.gaze.y) * gk;
    p.eyes.dx += this.gaze.x * 5;
    p.eyes.dy += this.gaze.y * 3;

    // ---- talk layer ----
    const tk = 1 - Math.exp(-dt * (this.talkTarget > this.talkLevel ? 30 : 10));
    this.talkLevel += (this.talkTarget - this.talkLevel) * tk;
    if (this.talking || this.talkLevel > 0.01) {
      const l = this.talkLevel;
      p.body.sy += l * 0.05;
      p.body.sx += l * 0.02;
      p.body.y -= l * 2.5;
      p.body.rot += Math.sin(this.time * 9) * l * 1.5;
      p.armL.rot += Math.sin(this.time * 7.3) * l * 6;
      p.armR.rot -= Math.sin(this.time * 6.7) * l * 6;
    }

    // ---- listening glow ----
    const glowTarget = this.listening ? 0.65 + Math.sin(this.time * 3) * 0.2 : 0;
    this.glowLevel += (glowTarget - this.glowLevel) * (1 - Math.exp(-dt * 6));
    p.glow = this.glowLevel;
    if (this.listening) {
      p.body.rot += 2; // attentive head tilt
      p.eyes.size *= 1.06;
    }

    // ---- idle fidgets ----
    if (!this.playback && !this.talking && !this.listening) {
      this.idleFor += dt;
      if (this.idleFor > 8 && this.onFidget) {
        this.idleFor = 0;
        const pick = this.onFidget();
        if (pick) void this.play(pick);
      }
    } else {
      this.idleFor = 0;
    }

    // ---- action layer (additive) ----
    if (this.playback) {
      const pb = this.playback;
      pb.elapsed += dt * 1000;
      let t = pb.elapsed / pb.def.duration;
      if (t >= 1) {
        if (pb.def.loop) {
          pb.elapsed %= pb.def.duration;
          t = pb.elapsed / pb.def.duration;
        } else {
          t = 1;
        }
      }
      const tr = pb.def.tracks;
      if (tr['body.x']) p.body.x += sampleTrack(tr['body.x'], t);
      if (tr['body.y']) p.body.y += sampleTrack(tr['body.y'], t);
      if (tr['body.rot']) p.body.rot += sampleTrack(tr['body.rot'], t);
      if (tr['body.sx']) p.body.sx += sampleTrack(tr['body.sx'], t);
      if (tr['body.sy']) p.body.sy += sampleTrack(tr['body.sy'], t);
      if (tr['armL.rot']) p.armL.rot += sampleTrack(tr['armL.rot'], t);
      if (tr['armR.rot']) p.armR.rot += sampleTrack(tr['armR.rot'], t);
      if (tr['leg0']) p.legs[0] += sampleTrack(tr['leg0'], t);
      if (tr['leg1']) p.legs[1] += sampleTrack(tr['leg1'], t);
      if (tr['leg2']) p.legs[2] += sampleTrack(tr['leg2'], t);
      if (tr['leg3']) p.legs[3] += sampleTrack(tr['leg3'], t);
      if (tr['eyes.open']) blinkOpen = Math.min(blinkOpen, Math.max(0, 1 + sampleTrack(tr['eyes.open'], t)));
      if (tr['eyes.dx']) p.eyes.dx += sampleTrack(tr['eyes.dx'], t);
      if (tr['eyes.dy']) p.eyes.dy += sampleTrack(tr['eyes.dy'], t);
      if (tr['eyes.size']) p.eyes.size += sampleTrack(tr['eyes.size'], t);
      if (tr['glow']) p.glow = Math.max(p.glow, sampleTrack(tr['glow'], t));
      if (pb.def.eyeShape) p.eyes.shape = pb.def.eyeShape;
      if (!pb.def.loop && pb.elapsed >= pb.def.duration) {
        pb.resolve?.();
        this.playback = null;
      }
    }

    p.eyes.open = blinkOpen;
    return p;
  }
}
