/**
 * Browser-side snapshot of a turn that is still running.
 *
 * The gateway persists a conversation only when a turn ends, so a reload in
 * the middle of a turn would otherwise lose every token streamed so far. This
 * keeps the live partial answer (and reasoning) in localStorage, keyed by
 * conversation, so hydration can put it back on screen and keep showing the
 * turn as running until the detached server-side turn finishes.
 */

const PREFIX = 'zeroclaw_inflight_turn_v1:';

export interface InflightTurn {
  content: string;
  thinking: string;
  /** Wall-clock time of the snapshot; used to drop stale entries. */
  updatedAt: number;
}

function key(sessionId: string): string {
  return `${PREFIX}${sessionId}`;
}

export function saveInflightTurn(sessionId: string, turn: InflightTurn): void {
  try {
    localStorage.setItem(key(sessionId), JSON.stringify(turn));
  } catch {
    // Quota or private-mode failures must never break the chat.
  }
}

export function loadInflightTurn(sessionId: string): InflightTurn | null {
  try {
    const raw = localStorage.getItem(key(sessionId));
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<InflightTurn>;
    if (typeof parsed.content !== 'string' || typeof parsed.thinking !== 'string') {
      return null;
    }
    return {
      content: parsed.content,
      thinking: parsed.thinking,
      updatedAt: typeof parsed.updatedAt === 'number' ? parsed.updatedAt : 0,
    };
  } catch {
    return null;
  }
}

export function clearInflightTurn(sessionId: string): void {
  try {
    localStorage.removeItem(key(sessionId));
  } catch {
    // ignore
  }
}
