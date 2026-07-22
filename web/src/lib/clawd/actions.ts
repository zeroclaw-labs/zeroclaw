/**
 * Clawd action library — named keyframe clips for the animation engine.
 *
 * All values are deltas from the rest pose. Rotation in degrees, distances
 * in rig units (the rig viewBox is 240×200 with the body ~150 wide).
 */
import type { ActionDef, Keyframe } from './engine';

const kf = (frames: Keyframe[]): Keyframe[] => frames;

/** Build a shiver-like oscillation track. */
function osc(cycles: number, amp: number, decay = false): Keyframe[] {
  const frames: Keyframe[] = [[0, 0]];
  const steps = cycles * 2;
  for (let i = 1; i <= steps; i++) {
    const t = i / (steps + 1);
    const a = decay ? amp * (1 - t) : amp;
    frames.push([t, i % 2 === 0 ? a : -a, 'inOutQuad']);
  }
  frames.push([1, 0, 'inOutQuad']);
  return frames;
}

export const ACTIONS: Record<string, ActionDef> = {
  // ============ greetings & social ============
  wave: {
    duration: 1400,
    eyeShape: 'happy',
    tracks: {
      'armR.rot': kf([[0, 0], [0.2, -150, 'outBack'], [0.35, -110], [0.5, -155], [0.65, -110], [0.8, -150], [1, 0, 'inOutCubic']]),
      'body.rot': kf([[0, 0], [0.2, -4, 'outQuad'], [0.8, -4], [1, 0]]),
    },
  },
  waveLeft: {
    duration: 1400,
    eyeShape: 'happy',
    tracks: {
      'armL.rot': kf([[0, 0], [0.2, 150, 'outBack'], [0.35, 110], [0.5, 155], [0.65, 110], [0.8, 150], [1, 0, 'inOutCubic']]),
      'body.rot': kf([[0, 0], [0.2, 4, 'outQuad'], [0.8, 4], [1, 0]]),
    },
  },
  waveBoth: {
    duration: 1600,
    eyeShape: 'happy',
    tracks: {
      'armL.rot': kf([[0, 0], [0.15, 150, 'outBack'], [0.35, 115], [0.55, 155], [0.75, 115], [1, 0, 'inOutCubic']]),
      'armR.rot': kf([[0, 0], [0.15, -150, 'outBack'], [0.35, -115], [0.55, -155], [0.75, -115], [1, 0, 'inOutCubic']]),
      'body.y': kf([[0, 0], [0.15, -4, 'outQuad'], [0.85, -4], [1, 0]]),
    },
  },
  salute: {
    duration: 1300,
    tracks: {
      'armR.rot': kf([[0, 0], [0.25, -140, 'outBack'], [0.75, -140], [1, 0, 'inOutCubic']]),
      'body.sy': kf([[0, 0], [0.25, 0.03, 'outQuad'], [0.75, 0.03], [1, 0]]),
      'eyes.dy': kf([[0, 0], [0.25, -1], [0.75, -1], [1, 0]]),
    },
  },
  bow: {
    duration: 1800,
    eyeShape: 'closed',
    tracks: {
      'body.rot': kf([[0, 0], [0.3, 18, 'inOutCubic'], [0.65, 18], [1, 0, 'inOutCubic']]),
      'body.y': kf([[0, 0], [0.3, 6, 'inOutCubic'], [0.65, 6], [1, 0, 'inOutCubic']]),
      'armL.rot': kf([[0, 0], [0.3, 25], [0.65, 25], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.3, -25], [0.65, -25], [1, 0]]),
    },
  },
  highFive: {
    duration: 900,
    eyeShape: 'happy',
    tracks: {
      'armR.rot': kf([[0, 0], [0.3, -160, 'outBack'], [0.45, -150], [1, 0, 'inOutCubic']]),
      'body.x': kf([[0, 0], [0.3, 6, 'outQuad'], [1, 0]]),
      'body.rot': kf([[0, 0], [0.3, -6], [1, 0]]),
    },
  },
  fistPump: {
    duration: 1100,
    eyeShape: 'squint',
    tracks: {
      'armR.rot': kf([[0, 0], [0.25, -170, 'outBack'], [0.5, -150], [0.7, -170], [1, 0, 'inOutCubic']]),
      'body.y': kf([[0, 0], [0.25, -8, 'outQuad'], [0.7, -8], [1, 0, 'outBounce']]),
      'body.sy': kf([[0, 0], [0.2, 0.06], [1, 0]]),
    },
  },
  point: {
    duration: 1400,
    tracks: {
      'armR.rot': kf([[0, 0], [0.25, -95, 'outBack'], [0.8, -95], [1, 0, 'inOutCubic']]),
      'eyes.dx': kf([[0, 0], [0.25, 5], [0.8, 5], [1, 0]]),
      'body.rot': kf([[0, 0], [0.25, -3], [0.8, -3], [1, 0]]),
    },
  },
  shrug: {
    duration: 1500,
    tracks: {
      'armL.rot': kf([[0, 0], [0.3, 65, 'outBack'], [0.75, 65], [1, 0, 'inOutCubic']]),
      'armR.rot': kf([[0, 0], [0.3, -65, 'outBack'], [0.75, -65], [1, 0, 'inOutCubic']]),
      'body.sy': kf([[0, 0], [0.3, 0.05, 'outQuad'], [0.75, 0.05], [1, 0]]),
      'body.rot': kf([[0, 0], [0.3, 3], [0.75, 3], [1, 0]]),
    },
  },
  // ============ locomotion ============
  rollLeft: {
    duration: 1600,
    eyeShape: 'squint',
    tracks: {
      'body.rot': kf([[0, 0], [1, -360, 'inOutCubic']]),
      'body.x': kf([[0, 0], [0.5, -55, 'inOutQuad'], [1, 0, 'inOutQuad']]),
      'body.y': kf([[0, 0], [0.13, -8, 'outQuad'], [0.25, 0, 'inQuad'], [0.38, -8, 'outQuad'], [0.5, 0, 'inQuad'], [0.63, -8, 'outQuad'], [0.75, 0, 'inQuad'], [0.88, -8, 'outQuad'], [1, 0, 'inQuad']]),
      'armL.rot': kf([[0, 0], [0.1, 40], [0.9, 40], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.1, -40], [0.9, -40], [1, 0]]),
    },
  },
  rollRight: {
    duration: 1600,
    eyeShape: 'squint',
    tracks: {
      'body.rot': kf([[0, 0], [1, 360, 'inOutCubic']]),
      'body.x': kf([[0, 0], [0.5, 55, 'inOutQuad'], [1, 0, 'inOutQuad']]),
      'body.y': kf([[0, 0], [0.13, -8, 'outQuad'], [0.25, 0, 'inQuad'], [0.38, -8, 'outQuad'], [0.5, 0, 'inQuad'], [0.63, -8, 'outQuad'], [0.75, 0, 'inQuad'], [0.88, -8, 'outQuad'], [1, 0, 'inQuad']]),
      'armL.rot': kf([[0, 0], [0.1, 40], [0.9, 40], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.1, -40], [0.9, -40], [1, 0]]),
    },
  },
  jump: {
    duration: 900,
    tracks: {
      'body.sy': kf([[0, 0], [0.2, -0.12, 'inOutQuad'], [0.35, 0.08, 'outQuad'], [0.75, 0.05], [0.9, -0.06, 'inQuad'], [1, 0, 'outQuad']]),
      'body.y': kf([[0, 0], [0.2, 4, 'inOutQuad'], [0.55, -30, 'outCubic'], [0.9, 0, 'inQuad'], [1, 0]]),
      'leg0': kf([[0, 0], [0.55, 6], [0.9, 0]]),
      'leg1': kf([[0, 0], [0.55, 8], [0.9, 0]]),
      'leg2': kf([[0, 0], [0.55, 8], [0.9, 0]]),
      'leg3': kf([[0, 0], [0.55, 6], [0.9, 0]]),
    },
  },
  doubleJump: {
    duration: 1500,
    eyeShape: 'happy',
    tracks: {
      'body.y': kf([[0, 0], [0.15, 3, 'inQuad'], [0.35, -24, 'outCubic'], [0.5, 0, 'inQuad'], [0.6, 2], [0.78, -32, 'outCubic'], [0.95, 0, 'inQuad'], [1, 0]]),
      'body.sy': kf([[0, 0], [0.12, -0.1], [0.3, 0.06], [0.5, -0.08], [0.72, 0.07], [1, 0]]),
    },
  },
  hop: {
    duration: 550,
    tracks: {
      'body.y': kf([[0, 0], [0.15, 2, 'inQuad'], [0.5, -14, 'outQuad'], [0.85, 0, 'inQuad'], [1, 0]]),
      'body.sy': kf([[0, 0], [0.12, -0.06], [0.4, 0.04], [1, 0]]),
    },
  },
  bounce: {
    duration: 1400,
    loop: true,
    tracks: {
      'body.y': kf([[0, 0], [0.25, -10, 'outQuad'], [0.5, 0, 'inQuad'], [0.75, -10, 'outQuad'], [1, 0, 'inQuad']]),
      'body.sy': kf([[0, 0], [0.05, -0.05], [0.25, 0.04], [0.5, -0.05], [0.75, 0.04], [1, -0.05]]),
    },
  },
  walkLeft: {
    duration: 1200,
    tracks: {
      'body.x': kf([[0, 0], [0.5, -30, 'inOutQuad'], [1, -30]]),
      'body.rot': osc(3, 3),
      'leg0': osc(3, -4),
      'leg2': osc(3, -4),
      'leg1': osc(3, 4),
      'leg3': osc(3, 4),
    },
  },
  walkRight: {
    duration: 1200,
    tracks: {
      'body.x': kf([[0, 0], [0.5, 30, 'inOutQuad'], [1, 30]]),
      'body.rot': osc(3, -3),
      'leg0': osc(3, 4),
      'leg2': osc(3, 4),
      'leg1': osc(3, -4),
      'leg3': osc(3, -4),
    },
  },
  walkBack: {
    duration: 1200,
    tracks: {
      'body.x': kf([[0, -30], [0.5, 0, 'inOutQuad'], [1, 0]]),
      'body.rot': osc(3, 2),
      'leg1': osc(3, 3),
      'leg2': osc(3, -3),
    },
  },
  run: {
    duration: 1000,
    eyeShape: 'squint',
    tracks: {
      'body.x': kf([[0, 0], [0.5, 45, 'inOutQuad'], [0.51, -45, 'step'], [1, 0, 'inOutQuad']]),
      'body.rot': kf([[0, 0], [0.1, 8], [0.9, 8], [1, 0]]),
      'leg0': osc(6, 5),
      'leg1': osc(6, -5),
      'leg2': osc(6, 5),
      'leg3': osc(6, -5),
    },
  },
  moonwalk: {
    duration: 1800,
    eyeShape: 'suspicious',
    tracks: {
      'body.x': kf([[0, 0], [0.8, 50, 'linear'], [1, 0, 'inOutQuad']]),
      'body.rot': kf([[0, 0], [0.1, -5], [0.8, -5], [1, 0]]),
      'leg0': osc(5, 5),
      'leg2': osc(5, 5),
      'leg1': osc(5, -5),
      'leg3': osc(5, -5),
      'body.y': osc(5, -1.5),
    },
  },
  scoot: {
    duration: 1000,
    tracks: {
      'body.x': kf([[0, 0], [0.25, 12, 'outQuad'], [0.5, 12], [0.75, 24, 'outQuad'], [1, 0, 'inOutQuad']]),
      'body.sy': kf([[0, 0], [0.15, -0.06], [0.3, 0.03], [0.6, -0.06], [0.8, 0.03], [1, 0]]),
    },
  },
  stomp: {
    duration: 800,
    eyeShape: 'angry',
    tracks: {
      'leg1': kf([[0, 0], [0.25, -10, 'outQuad'], [0.45, 0, 'inQuad'], [1, 0]]),
      'body.y': kf([[0, 0], [0.45, 0], [0.5, 3, 'outQuad'], [0.65, 0], [1, 0]]),
      'body.rot': kf([[0, 0], [0.25, -3], [0.5, 0], [1, 0]]),
    },
  },
  tapFoot: {
    duration: 1200,
    loop: true,
    eyeShape: 'suspicious',
    tracks: {
      'leg3': kf([[0, 0], [0.2, -6, 'outQuad'], [0.4, 0, 'inQuad'], [0.6, -6, 'outQuad'], [0.8, 0, 'inQuad'], [1, 0]]),
      'body.rot': kf([[0, 2], [1, 2]]),
      'eyes.dx': kf([[0, 4], [1, 4]]),
    },
  },
  // ============ flips & spins ============
  spin: {
    duration: 1100,
    tracks: {
      'body.rot': kf([[0, 0], [1, 360, 'inOutCubic']]),
      'body.sy': kf([[0, 0], [0.15, -0.06], [0.5, 0.04], [1, 0]]),
    },
  },
  spinJump: {
    duration: 1300,
    eyeShape: 'happy',
    tracks: {
      'body.rot': kf([[0, 0], [0.2, 0], [0.8, 360, 'inOutCubic'], [1, 360]]),
      'body.y': kf([[0, 0], [0.2, 4, 'inQuad'], [0.55, -36, 'outCubic'], [0.9, 0, 'inQuad'], [1, 0]]),
      'body.sy': kf([[0, 0], [0.15, -0.1], [0.35, 0.06], [0.9, 0], [0.95, -0.05], [1, 0]]),
    },
  },
  backflip: {
    duration: 1400,
    eyeShape: 'squint',
    tracks: {
      'body.rot': kf([[0, 0], [0.25, 0], [0.85, -360, 'inOutCubic'], [1, -360]]),
      'body.y': kf([[0, 0], [0.25, 5, 'inQuad'], [0.6, -42, 'outCubic'], [0.95, 0, 'inQuad'], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.25, 30], [0.85, 30], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.25, -30], [0.85, -30], [1, 0]]),
    },
  },
  pirouette: {
    duration: 2000,
    eyeShape: 'closed',
    tracks: {
      'body.rot': kf([[0, 0], [0.7, 720, 'inOutCubic'], [1, 720]]),
      'body.sx': kf([[0, 0], [0.3, -0.08], [0.7, -0.08], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.2, 170, 'outBack'], [0.8, 170], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.2, -170, 'outBack'], [0.8, -170], [1, 0]]),
    },
  },
  // ============ emotive bursts ============
  nod: {
    duration: 900,
    tracks: {
      'body.rot': kf([[0, 0], [0.2, 8, 'inOutQuad'], [0.45, -2], [0.7, 8], [1, 0, 'inOutQuad']]),
      'eyes.dy': kf([[0, 0], [0.2, 2], [0.45, 0], [0.7, 2], [1, 0]]),
    },
  },
  shakeHead: {
    duration: 900,
    tracks: {
      'body.rot': osc(3, 7, true),
      'eyes.dx': osc(3, 4, true),
    },
  },
  cheer: {
    duration: 1600,
    eyeShape: 'star',
    tracks: {
      'armL.rot': kf([[0, 0], [0.2, 160, 'outBack'], [0.5, 140], [0.7, 160], [1, 0, 'inOutCubic']]),
      'armR.rot': kf([[0, 0], [0.2, -160, 'outBack'], [0.5, -140], [0.7, -160], [1, 0, 'inOutCubic']]),
      'body.y': kf([[0, 0], [0.25, -18, 'outQuad'], [0.5, 0, 'inQuad'], [0.65, -14, 'outQuad'], [0.85, 0, 'inQuad'], [1, 0]]),
    },
  },
  celebrate: {
    duration: 2400,
    eyeShape: 'star',
    tracks: {
      'body.rot': kf([[0, 0], [0.3, 360, 'inOutCubic'], [0.55, 360], [0.7, 352], [0.85, 368], [1, 360]]),
      'body.y': kf([[0, 0], [0.15, 4], [0.4, -30, 'outCubic'], [0.6, 0, 'inQuad'], [0.75, -12, 'outQuad'], [0.9, 0, 'inQuad'], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.55, 0], [0.65, 160, 'outBack'], [0.9, 160], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.55, 0], [0.65, -160, 'outBack'], [0.9, -160], [1, 0]]),
      'glow': kf([[0, 0], [0.4, 1], [0.9, 1], [1, 0]]),
    },
  },
  dance: {
    duration: 2000,
    loop: true,
    eyeShape: 'happy',
    tracks: {
      'body.y': kf([[0, 0], [0.125, -8, 'outQuad'], [0.25, 0, 'inQuad'], [0.375, -8, 'outQuad'], [0.5, 0, 'inQuad'], [0.625, -8, 'outQuad'], [0.75, 0, 'inQuad'], [0.875, -8, 'outQuad'], [1, 0, 'inQuad']]),
      'body.rot': kf([[0, -6], [0.25, 6, 'inOutQuad'], [0.5, -6, 'inOutQuad'], [0.75, 6, 'inOutQuad'], [1, -6, 'inOutQuad']]),
      'armL.rot': kf([[0, 40], [0.25, 120, 'inOutQuad'], [0.5, 40, 'inOutQuad'], [0.75, 120, 'inOutQuad'], [1, 40, 'inOutQuad']]),
      'armR.rot': kf([[0, -120], [0.25, -40, 'inOutQuad'], [0.5, -120, 'inOutQuad'], [0.75, -40, 'inOutQuad'], [1, -120, 'inOutQuad']]),
    },
  },
  headbang: {
    duration: 800,
    loop: true,
    eyeShape: 'closed',
    tracks: {
      'body.rot': kf([[0, -10], [0.25, 14, 'inQuad'], [0.5, -10, 'outQuad'], [0.75, 14, 'inQuad'], [1, -10, 'outQuad']]),
      'body.y': kf([[0, 0], [0.25, 3], [0.5, 0], [0.75, 3], [1, 0]]),
    },
  },
  wiggle: {
    duration: 1000,
    tracks: {
      'body.rot': osc(4, 6, true),
      'body.sx': osc(4, 0.03, true),
    },
  },
  wobble: {
    duration: 1400,
    tracks: {
      'body.rot': kf([[0, 0], [0.15, 14, 'outQuad'], [0.35, -11, 'inOutQuad'], [0.55, 8, 'inOutQuad'], [0.72, -5, 'inOutQuad'], [0.86, 2, 'inOutQuad'], [1, 0, 'inOutQuad']]),
    },
  },
  shiver: {
    duration: 900,
    eyeShape: 'squint',
    tracks: {
      'body.x': osc(9, 2),
      'armL.rot': osc(9, 4),
      'armR.rot': osc(9, -4),
    },
  },
  laugh: {
    duration: 1600,
    eyeShape: 'squint',
    tracks: {
      'body.sy': kf([[0, 0], [0.1, 0.06], [0.2, -0.02], [0.3, 0.06], [0.4, -0.02], [0.5, 0.06], [0.6, -0.02], [0.7, 0.06], [0.8, -0.02], [1, 0]]),
      'body.y': kf([[0, 0], [0.1, -4], [0.2, 1], [0.3, -4], [0.4, 1], [0.5, -4], [0.6, 1], [0.7, -4], [0.8, 1], [1, 0]]),
      'body.rot': kf([[0, 0], [0.15, -5, 'outQuad'], [0.85, -5], [1, 0]]),
    },
  },
  giggle: {
    duration: 900,
    eyeShape: 'happy',
    tracks: {
      'body.sy': osc(5, 0.03, true),
      'body.rot': kf([[0, 0], [0.2, 4], [0.8, 4], [1, 0]]),
    },
  },
  gasp: {
    duration: 1100,
    eyeShape: 'o',
    tracks: {
      'body.sy': kf([[0, 0], [0.12, 0.09, 'outBack'], [0.7, 0.09], [1, 0, 'inOutQuad']]),
      'body.y': kf([[0, 0], [0.12, -6, 'outBack'], [0.7, -6], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.12, 60, 'outBack'], [0.7, 60], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.12, -60, 'outBack'], [0.7, -60], [1, 0]]),
      'eyes.size': kf([[0, 0], [0.12, 0.25], [0.7, 0.25], [1, 0]]),
    },
  },
  surprise: {
    duration: 1000,
    eyeShape: 'o',
    tracks: {
      'body.y': kf([[0, 0], [0.1, -12, 'outBack'], [0.5, -8], [1, 0, 'inOutQuad']]),
      'body.sy': kf([[0, 0], [0.1, 0.1, 'outBack'], [1, 0]]),
      'eyes.size': kf([[0, 0], [0.1, 0.3], [0.6, 0.3], [1, 0]]),
    },
  },
  pout: {
    duration: 1600,
    eyeShape: 'sad',
    tracks: {
      'body.rot': kf([[0, 0], [0.25, 5, 'inOutQuad'], [0.8, 5], [1, 0]]),
      'body.y': kf([[0, 0], [0.25, 4], [0.8, 4], [1, 0]]),
      'eyes.dx': kf([[0, 0], [0.25, -5], [0.8, -5], [1, 0]]),
      'eyes.dy': kf([[0, 0], [0.25, 2], [0.8, 2], [1, 0]]),
    },
  },
  cry: {
    duration: 2000,
    eyeShape: 'sad',
    tracks: {
      'body.sy': osc(6, -0.02),
      'body.y': kf([[0, 0], [0.2, 4], [0.8, 4], [1, 0]]),
      'eyes.dy': kf([[0, 0], [0.2, 3], [0.8, 3], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.3, 80, 'inOutQuad'], [0.7, 80], [1, 0]]),
    },
  },
  angry: {
    duration: 1500,
    eyeShape: 'angry',
    tracks: {
      'body.x': osc(6, 1.5, true),
      'body.sy': kf([[0, 0], [0.15, -0.04], [0.85, -0.04], [1, 0]]),
      'glow': kf([[0, 0], [0.2, 0.5], [0.8, 0.5], [1, 0]]),
    },
  },
  facepalm: {
    duration: 1800,
    eyeShape: 'closed',
    tracks: {
      'armR.rot': kf([[0, 0], [0.25, -130, 'outQuad'], [0.75, -130], [1, 0, 'inOutCubic']]),
      'body.rot': kf([[0, 0], [0.25, 6, 'inOutQuad'], [0.75, 6], [1, 0]]),
      'body.y': kf([[0, 0], [0.25, 3], [0.75, 3], [1, 0]]),
    },
  },
  // ============ cognition ============
  think: {
    duration: 2400,
    loop: true,
    eyeShape: 'suspicious',
    tracks: {
      'body.rot': kf([[0, -3], [0.5, -5, 'inOutQuad'], [1, -3, 'inOutQuad']]),
      'armL.rot': kf([[0, 55], [1, 55]]),
      'eyes.dx': kf([[0, -4], [0.5, -5], [1, -4]]),
      'eyes.dy': kf([[0, -3], [1, -3]]),
    },
  },
  hmm: {
    duration: 1500,
    eyeShape: 'suspicious',
    tracks: {
      'body.rot': kf([[0, 0], [0.3, -6, 'inOutQuad'], [0.8, -6], [1, 0]]),
      'eyes.dx': kf([[0, 0], [0.3, -5], [0.6, 5, 'inOutQuad'], [0.8, -3], [1, 0]]),
      'eyes.dy': kf([[0, 0], [0.3, -3], [0.8, -3], [1, 0]]),
    },
  },
  confused: {
    duration: 1600,
    eyeShape: 'rect',
    tracks: {
      'body.rot': kf([[0, 0], [0.25, 10, 'inOutQuad'], [0.55, -8, 'inOutQuad'], [0.8, 10, 'inOutQuad'], [1, 0]]),
      'eyes.dx': kf([[0, 0], [0.25, 5], [0.55, -5], [0.8, 5], [1, 0]]),
      'eyes.size': kf([[0, 0], [0.2, 0.1], [0.8, 0.1], [1, 0]]),
    },
  },
  dizzy: {
    duration: 2000,
    eyeShape: 'dizzy',
    tracks: {
      'body.rot': kf([[0, 0], [0.2, 8, 'inOutQuad'], [0.4, -8, 'inOutQuad'], [0.6, 6, 'inOutQuad'], [0.8, -4, 'inOutQuad'], [1, 0]]),
      'body.x': kf([[0, 0], [0.2, -4], [0.4, 4], [0.6, -3], [0.8, 2], [1, 0]]),
      'eyes.dy': kf([[0, 0], [0.5, 2], [1, 0]]),
    },
  },
  idea: {
    duration: 1200,
    eyeShape: 'wide',
    tracks: {
      'body.y': kf([[0, 0], [0.15, -10, 'outBack'], [0.6, -8], [1, 0, 'inOutQuad']]),
      'armR.rot': kf([[0, 0], [0.15, -150, 'outBack'], [0.6, -150], [1, 0]]),
      'glow': kf([[0, 0], [0.15, 1, 'outQuad'], [0.7, 0.8], [1, 0]]),
      'eyes.size': kf([[0, 0], [0.15, 0.2], [0.7, 0.2], [1, 0]]),
    },
  },
  search: {
    duration: 2600,
    tracks: {
      'eyes.dx': kf([[0, 0], [0.2, -6, 'inOutQuad'], [0.45, -6], [0.65, 6, 'inOutQuad'], [0.85, 6], [1, 0]]),
      'body.rot': kf([[0, 0], [0.2, 5, 'inOutQuad'], [0.45, 5], [0.65, -5, 'inOutQuad'], [0.85, -5], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.2, 45], [0.85, 45], [1, 0]]),
    },
  },
  // ============ gaze ============
  lookLeft: {
    duration: 1400,
    tracks: {
      'eyes.dx': kf([[0, 0], [0.2, -6, 'outQuad'], [0.8, -6], [1, 0]]),
      'body.rot': kf([[0, 0], [0.2, 3], [0.8, 3], [1, 0]]),
    },
  },
  lookRight: {
    duration: 1400,
    tracks: {
      'eyes.dx': kf([[0, 0], [0.2, 6, 'outQuad'], [0.8, 6], [1, 0]]),
      'body.rot': kf([[0, 0], [0.2, -3], [0.8, -3], [1, 0]]),
    },
  },
  lookUp: {
    duration: 1400,
    tracks: {
      'eyes.dy': kf([[0, 0], [0.2, -5, 'outQuad'], [0.8, -5], [1, 0]]),
      'body.sy': kf([[0, 0], [0.2, 0.03], [0.8, 0.03], [1, 0]]),
    },
  },
  lookDown: {
    duration: 1400,
    tracks: {
      'eyes.dy': kf([[0, 0], [0.2, 4, 'outQuad'], [0.8, 4], [1, 0]]),
      'body.rot': kf([[0, 0], [0.2, 2], [0.8, 2], [1, 0]]),
    },
  },
  lookAround: {
    duration: 2800,
    tracks: {
      'eyes.dx': kf([[0, 0], [0.15, -6, 'outQuad'], [0.35, -6], [0.5, 6, 'inOutQuad'], [0.7, 6], [0.85, 0, 'inOutQuad'], [1, 0]]),
      'eyes.dy': kf([[0, 0], [0.5, -2], [0.85, 0], [1, 0]]),
      'body.rot': kf([[0, 0], [0.15, 4, 'outQuad'], [0.5, -4, 'inOutQuad'], [0.85, 0, 'inOutQuad'], [1, 0]]),
    },
  },
  doubleTake: {
    duration: 1300,
    tracks: {
      'eyes.dx': kf([[0, 0], [0.15, 6, 'outQuad'], [0.3, 0, 'inOutQuad'], [0.45, 6, 'outCubic'], [0.85, 6], [1, 0]]),
      'body.rot': kf([[0, 0], [0.45, -5, 'outBack'], [0.85, -5], [1, 0]]),
      'eyes.size': kf([[0, 0], [0.45, 0.2, 'outQuad'], [0.85, 0.2], [1, 0]]),
    },
  },
  sideEye: {
    duration: 2000,
    eyeShape: 'suspicious',
    tracks: {
      'eyes.dx': kf([[0, 0], [0.15, 6, 'outQuad'], [0.85, 6], [1, 0]]),
      'body.rot': kf([[0, 0], [0.15, 1], [0.85, 1], [1, 0]]),
    },
  },
  peek: {
    duration: 2200,
    tracks: {
      'body.x': kf([[0, 0], [0.25, -70, 'inOutCubic'], [0.4, -45, 'outBack'], [0.75, -45], [1, 0, 'inOutCubic']]),
      'body.rot': kf([[0, 0], [0.4, 8, 'outQuad'], [0.75, 8], [1, 0]]),
      'eyes.dx': kf([[0, 0], [0.4, 6], [0.75, 6], [1, 0]]),
    },
  },
  hide: {
    duration: 2600,
    eyeShape: 'squint',
    tracks: {
      'body.y': kf([[0, 0], [0.25, 90, 'inBack'], [0.7, 90], [1, 0, 'outBack']]),
    },
  },
  // ============ states & rituals ============
  yawn: {
    duration: 2200,
    eyeShape: 'closed',
    tracks: {
      'body.sy': kf([[0, 0], [0.3, 0.08, 'inOutQuad'], [0.55, 0.08], [0.75, -0.04, 'inOutQuad'], [1, 0]]),
      'body.rot': kf([[0, 0], [0.3, -4], [0.55, -4], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.3, 120, 'inOutQuad'], [0.55, 120], [1, 0, 'inOutQuad']]),
      'armR.rot': kf([[0, 0], [0.3, -60, 'inOutQuad'], [0.55, -60], [1, 0]]),
    },
  },
  stretch: {
    duration: 2000,
    eyeShape: 'squint',
    tracks: {
      'body.sy': kf([[0, 0], [0.35, 0.12, 'inOutCubic'], [0.65, 0.12], [1, 0, 'inOutQuad']]),
      'body.sx': kf([[0, 0], [0.35, -0.06, 'inOutCubic'], [0.65, -0.06], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.35, 160, 'inOutCubic'], [0.65, 160], [1, 0, 'inOutQuad']]),
      'armR.rot': kf([[0, 0], [0.35, -160, 'inOutCubic'], [0.65, -160], [1, 0, 'inOutQuad']]),
    },
  },
  squashJelly: {
    duration: 1200,
    eyeShape: 'squint',
    tracks: {
      'body.sy': kf([[0, 0], [0.2, -0.25, 'outQuad'], [0.45, 0.12, 'outElastic'], [0.7, -0.05], [1, 0, 'outQuad']]),
      'body.sx': kf([[0, 0], [0.2, 0.15, 'outQuad'], [0.45, -0.08, 'outElastic'], [1, 0]]),
      'body.y': kf([[0, 0], [0.2, 8, 'outQuad'], [0.45, -4], [1, 0]]),
    },
  },
  sleep: {
    duration: 3200,
    loop: true,
    eyeShape: 'closed',
    tracks: {
      'body.sy': kf([[0, 0], [0.5, 0.035, 'inOutQuad'], [1, 0, 'inOutQuad']]),
      'body.rot': kf([[0, 6], [1, 6]]),
      'body.y': kf([[0, 4], [1, 4]]),
    },
  },
  wakeUp: {
    duration: 1600,
    tracks: {
      'body.rot': kf([[0, 6], [0.3, 6], [0.5, -3, 'outBack'], [0.7, 2], [1, 0]]),
      'body.y': kf([[0, 4], [0.3, 4], [0.5, -6, 'outBack'], [1, 0]]),
      'body.sy': kf([[0, 0], [0.5, 0.08, 'outQuad'], [0.75, 0.08], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.5, 140, 'inOutQuad'], [0.75, 140], [1, 0, 'inOutQuad']]),
      'armR.rot': kf([[0, 0], [0.5, -140, 'inOutQuad'], [0.75, -140], [1, 0, 'inOutQuad']]),
    },
  },
  sneeze: {
    duration: 1300,
    tracks: {
      'body.sy': kf([[0, 0], [0.25, 0.1, 'inOutQuad'], [0.4, 0.12], [0.45, -0.18, 'inCubic'], [0.6, 0.05, 'outQuad'], [1, 0]]),
      'body.rot': kf([[0, 0], [0.25, -6], [0.45, 14, 'inCubic'], [0.65, 0, 'outQuad'], [1, 0]]),
      'body.y': kf([[0, 0], [0.45, 4, 'inCubic'], [0.6, 0], [1, 0]]),
      'eyes.open': kf([[0, 0], [0.25, 0], [0.4, -1, 'outQuad'], [0.7, -1], [1, 0]]),
    },
  },
  typeFuriously: {
    duration: 1800,
    loop: true,
    eyeShape: 'rect',
    tracks: {
      'armL.rot': osc(10, 12),
      'armR.rot': osc(10, -12),
      'body.rot': kf([[0, 3], [1, 3]]),
      'eyes.dy': kf([[0, 3], [1, 3]]),
    },
  },
  nervous: {
    duration: 1800,
    tracks: {
      'eyes.dx': kf([[0, 0], [0.15, -5, 'outQuad'], [0.3, 5, 'inOutQuad'], [0.45, -4], [0.6, 5], [0.8, -3], [1, 0]]),
      'body.x': osc(8, 1),
    },
  },
  // ============ affection & flair ============
  heartEyes: {
    duration: 2200,
    eyeShape: 'heart',
    tracks: {
      'body.sy': kf([[0, 0], [0.2, 0.05, 'outBack'], [0.5, 0.02], [0.8, 0.05], [1, 0]]),
      'body.rot': osc(3, 2),
      'eyes.size': kf([[0, 0], [0.2, 0.3, 'outBack'], [0.8, 0.3], [1, 0]]),
      'glow': kf([[0, 0], [0.2, 0.7], [0.8, 0.7], [1, 0]]),
    },
  },
  starEyes: {
    duration: 2000,
    eyeShape: 'star',
    tracks: {
      'eyes.size': kf([[0, 0], [0.15, 0.35, 'outBack'], [0.85, 0.35], [1, 0]]),
      'body.y': kf([[0, 0], [0.15, -4, 'outQuad'], [0.85, -4], [1, 0]]),
      'glow': kf([[0, 0], [0.15, 0.8], [0.85, 0.8], [1, 0]]),
    },
  },
  wink: {
    duration: 900,
    eyeShape: 'wink',
    tracks: {
      'body.rot': kf([[0, 0], [0.25, -4, 'outQuad'], [0.75, -4], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.25, -50, 'outBack'], [0.75, -50], [1, 0]]),
    },
  },
  blowKiss: {
    duration: 1400,
    eyeShape: 'wink',
    tracks: {
      'armR.rot': kf([[0, 0], [0.25, -120, 'outQuad'], [0.45, -120], [0.6, -70, 'outBack'], [1, 0, 'inOutQuad']]),
      'body.rot': kf([[0, 0], [0.25, -3], [0.6, -6, 'outQuad'], [1, 0]]),
      'glow': kf([[0, 0], [0.55, 0], [0.65, 0.8, 'outQuad'], [1, 0]]),
    },
  },
  sparkle: {
    duration: 1600,
    eyeShape: 'sparkle',
    tracks: {
      'glow': kf([[0, 0], [0.2, 1, 'outQuad'], [0.8, 0.8], [1, 0]]),
      'eyes.size': kf([[0, 0], [0.2, 0.25], [0.8, 0.25], [1, 0]]),
      'body.y': osc(4, -1.5),
    },
  },
  powerUp: {
    duration: 1800,
    eyeShape: 'squint',
    tracks: {
      'body.sy': kf([[0, 0], [0.4, -0.1, 'inOutQuad'], [0.55, 0.15, 'outBack'], [0.8, 0.1], [1, 0]]),
      'body.y': kf([[0, 0], [0.4, 6, 'inOutQuad'], [0.55, -14, 'outBack'], [0.8, -10], [1, 0]]),
      'glow': kf([[0, 0], [0.4, 0.3, 'inQuad'], [0.6, 1], [0.9, 1], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.55, 150, 'outBack'], [0.85, 150], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.55, -150, 'outBack'], [0.85, -150], [1, 0]]),
    },
  },
  glitch: {
    duration: 700,
    eyeShape: 'x',
    tracks: {
      'body.x': kf([[0, 0], [0.1, -6, 'step'], [0.2, 5, 'step'], [0.3, -3, 'step'], [0.42, 6, 'step'], [0.55, -4, 'step'], [0.7, 2, 'step'], [0.85, -1, 'step'], [1, 0, 'step']]),
      'body.sx': kf([[0, 0], [0.15, 0.1, 'step'], [0.35, -0.08, 'step'], [0.6, 0.06, 'step'], [1, 0, 'step']]),
      'eyes.dx': kf([[0, 0], [0.2, 4, 'step'], [0.5, -4, 'step'], [1, 0, 'step']]),
    },
  },
  float: {
    duration: 3000,
    loop: true,
    tracks: {
      'body.y': kf([[0, 0], [0.5, -12, 'inOutQuad'], [1, 0, 'inOutQuad']]),
      'leg0': kf([[0, 0], [0.5, 3, 'inOutQuad'], [1, 0]]),
      'leg1': kf([[0, 0], [0.5, 5, 'inOutQuad'], [1, 0]]),
      'leg2': kf([[0, 0], [0.5, 4, 'inOutQuad'], [1, 0]]),
      'leg3': kf([[0, 0], [0.5, 6, 'inOutQuad'], [1, 0]]),
      'armL.rot': kf([[0, 0], [0.5, 20, 'inOutQuad'], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.5, -20, 'inOutQuad'], [1, 0]]),
    },
  },
  land: {
    duration: 700,
    tracks: {
      'body.y': kf([[0, -40], [0.4, 0, 'inQuad'], [1, 0]]),
      'body.sy': kf([[0, 0], [0.4, -0.15, 'outQuad'], [0.7, 0.06, 'outQuad'], [1, 0]]),
      'body.sx': kf([[0, 0], [0.4, 0.1, 'outQuad'], [0.7, -0.04], [1, 0]]),
    },
  },
  // ============ conversational beats ============
  greetSequence: {
    duration: 2600,
    eyeShape: 'happy',
    tracks: {
      'body.y': kf([[0, 60], [0.2, -8, 'outBack'], [0.3, 0], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.35, -150, 'outBack'], [0.5, -115], [0.65, -152], [0.8, -115], [1, 0, 'inOutCubic']]),
      'body.rot': kf([[0, 0], [0.35, -4], [0.9, -4], [1, 0]]),
    },
  },
  attention: {
    duration: 800,
    tracks: {
      'body.sy': kf([[0, 0], [0.2, 0.06, 'outBack'], [0.7, 0.06], [1, 0]]),
      'body.y': kf([[0, 0], [0.2, -4, 'outBack'], [0.7, -4], [1, 0]]),
      'eyes.size': kf([[0, 0], [0.2, 0.15], [0.7, 0.15], [1, 0]]),
    },
  },
  agree: {
    duration: 1300,
    eyeShape: 'happy',
    tracks: {
      'body.rot': kf([[0, 0], [0.15, 7, 'inOutQuad'], [0.35, -2], [0.55, 7], [0.75, -2], [1, 0]]),
    },
  },
  disagree: {
    duration: 1300,
    tracks: {
      'body.rot': osc(4, 6, true),
      'armL.rot': kf([[0, 0], [0.2, 40, 'outQuad'], [0.8, 40], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.2, -40, 'outQuad'], [0.8, -40], [1, 0]]),
    },
  },
  ponder: {
    duration: 3000,
    eyeShape: 'suspicious',
    tracks: {
      'body.rot': kf([[0, 0], [0.3, -5, 'inOutQuad'], [0.7, -5], [1, 0]]),
      'eyes.dy': kf([[0, 0], [0.3, -4], [0.7, -4], [1, 0]]),
      'eyes.dx': kf([[0, 0], [0.3, -3], [0.5, 3, 'inOutQuad'], [0.7, -3, 'inOutQuad'], [1, 0]]),
      'armR.rot': kf([[0, 0], [0.3, -60, 'inOutQuad'], [0.7, -60], [1, 0]]),
    },
  },
};

/** Actions safe to fire randomly while idle — short, subtle, non-locomotive.
 * Looping clips (tapFoot, bounce, …) are deliberately excluded: a fidget
 * must end on its own. */
export const IDLE_FIDGETS = [
  'lookAround',
  'wiggle',
  'hop',
  'stretch',
  'sideEye',
  'doubleTake',
  'wobble',
  'hmm',
  'scoot',
];

/** Map agent lifecycle → ambient clip. */
export const STATE_CLIPS = {
  thinking: 'think',
  working: 'typeFuriously',
  sleeping: 'sleep',
  dancing: 'dance',
} as const;
