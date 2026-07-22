/**
 * adaptiveFrameLoop — a self-throttling requestAnimationFrame driver.
 *
 * The mascot rig doesn't need 60fps to look alive, and busy tabs (many
 * chart-heavy dashboard panels open, a low-power laptop, a background CPU
 * spike) shouldn't fight the browser for frames. This driver keeps the
 * underlying loop pinned to `requestAnimationFrame` (so timing stays locked
 * to vsync and the browser's own throttling/back-pressure applies), but only
 * invokes the callback once enough time has elapsed for the *current* fps
 * tier — 60 / 30 / 20 — and watches recent behavior to pick the right tier:
 *
 *  - Every ~60 invoked frames form a window. A frame "misses" its window if
 *    the gap since the previous invocation exceeded 1.75x the tier's target
 *    interval (i.e. real jank, not just normal scheduling noise).
 *  - If missRate > 12% or the callback's own average duration > 9ms across
 *    the window, step down one tier (60→30→20) immediately — no hysteresis
 *    on the way down, since dropped frames are visible right away.
 *  - Stepping back up requires two consecutive *healthy* windows in a row
 *    (hysteresis), so a single good window doesn't flap the tier.
 *  - While `document.hidden`, the loop pauses entirely (no callback calls,
 *    no window bookkeeping). On resume it discards the stale "last frame"
 *    timestamp so the next invoked frame gets a fresh, small dt instead of
 *    a multi-second catch-up jump.
 *  - `dt` handed to the callback is always clamped to <= 0.05s, matching
 *    the previous fixed-rAF behavior.
 */

export type FrameCallback = (dtSeconds: number) => void;

/** fps tiers, fastest first; the loop only ever steps one tier at a time. */
const TIERS = [60, 30, 20] as const;
type TierIndex = 0 | 1 | 2;
const LAST_TIER: TierIndex = (TIERS.length - 1) as TierIndex;

const WINDOW_SIZE = 60;
const MISS_RATE_THRESHOLD = 0.12;
const AVG_WORK_MS_THRESHOLD = 9;
const HEALTHY_WINDOWS_TO_UPGRADE = 2;
/** A frame counts as "missed" once its actual gap exceeds 1.75x the target interval. */
const MISS_ELAPSED_FACTOR = 1.75;
const MAX_DT_SECONDS = 0.05;

function tierIntervalMs(tier: TierIndex): number {
  return 1000 / TIERS[tier];
}

/**
 * Start the adaptive loop. Returns a stop function that cancels the
 * underlying rAF.
 */
export function startAdaptiveFrameLoop(cb: FrameCallback): () => void {
  let tier: TierIndex = 0;
  let rafId = 0;
  let stopped = false;
  let last = 0;
  let hasLast = false;

  // Rolling window bookkeeping (counts invoked frames only).
  let framesInWindow = 0;
  let missesInWindow = 0;
  let workMsInWindow = 0;
  let healthyStreak = 0;

  const resetWindow = () => {
    framesInWindow = 0;
    missesInWindow = 0;
    workMsInWindow = 0;
  };

  const tick = (now: number) => {
    if (stopped) return;
    rafId = requestAnimationFrame(tick);

    // Pause state is DERIVED each tick, never latched from events: a missed
    // visibilitychange (macOS display sleep / App Nap / space switches) once
    // left a latched `paused=true` forever — the mascot froze mid-blink
    // until the tab was refocused. The browser already stops rAF while
    // hidden, so polling document.hidden here costs nothing and recovery is
    // automatic on the first frame after the page becomes visible again.
    if (typeof document !== 'undefined' && document.hidden) {
      hasLast = false;
      return;
    }

    if (!hasLast) {
      last = now;
      hasLast = true;
      return;
    }

    const elapsed = now - last;
    const target = tierIntervalMs(tier);
    if (elapsed < target) return;
    last = now;

    const dt = Math.min(MAX_DT_SECONDS, elapsed / 1000);

    const workStart = performance.now();
    cb(dt);
    const workMs = performance.now() - workStart;

    framesInWindow += 1;
    workMsInWindow += workMs;
    if (elapsed > target * MISS_ELAPSED_FACTOR) missesInWindow += 1;

    if (framesInWindow >= WINDOW_SIZE) {
      const missRate = missesInWindow / framesInWindow;
      const avgWorkMs = workMsInWindow / framesInWindow;
      const unhealthy = missRate > MISS_RATE_THRESHOLD || avgWorkMs > AVG_WORK_MS_THRESHOLD;

      if (unhealthy) {
        healthyStreak = 0;
        if (tier < LAST_TIER) tier = (tier + 1) as TierIndex;
      } else {
        healthyStreak += 1;
        if (healthyStreak >= HEALTHY_WINDOWS_TO_UPGRADE && tier > 0) {
          tier = (tier - 1) as TierIndex;
          healthyStreak = 0;
        }
      }
      resetWindow();
    }
  };

  rafId = requestAnimationFrame(tick);

  return () => {
    stopped = true;
    cancelAnimationFrame(rafId);
  };
}
