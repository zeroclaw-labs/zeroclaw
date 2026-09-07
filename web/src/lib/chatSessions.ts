import { generateUUID } from './uuid';

/**
 * Pointer to the chat session an agent alias is currently showing.
 *
 * The gateway keys every conversation by `session_id` (`gw_<id>` on the wire),
 * so several independent conversations can coexist for one agent. The web UI
 * needs to remember which one the operator was last looking at; that pointer is
 * the only session state the browser owns. Transcripts live on the gateway
 * (`/api/sessions`), with `chatHistoryStorage` acting as a per-session cache —
 * neither is duplicated here.
 */
const ACTIVE_SESSION_KEY_PREFIX = 'zeroclaw_active_session';

/**
 * Pre-multi-session key. It held one fixed session id per alias, which is
 * exactly what the pointer now holds, so it is adopted on first read and then
 * removed — the value moves, it is never mirrored in both places.
 */
const LEGACY_SESSION_ID_KEY_PREFIX = 'zeroclaw_session_id';

function activeKey(agentAlias: string): string {
  return `${ACTIVE_SESSION_KEY_PREFIX}.${agentAlias}`;
}

/** Mint an id for a brand-new conversation. */
export function newSessionId(): string {
  return generateUUID();
}

/**
 * Resolve the active session id for `agentAlias`, creating one when the agent
 * has never been opened. Adopts the pre-multi-session key when present so an
 * upgrade keeps showing the conversation the operator already had.
 */
export function getActiveSessionId(agentAlias: string): string {
  try {
    const existing = localStorage.getItem(activeKey(agentAlias));
    if (existing) return existing;

    const legacyKey = `${LEGACY_SESSION_ID_KEY_PREFIX}.${agentAlias}`;
    const legacy = localStorage.getItem(legacyKey);
    if (legacy) {
      localStorage.setItem(activeKey(agentAlias), legacy);
      localStorage.removeItem(legacyKey);
      return legacy;
    }

    const fresh = newSessionId();
    localStorage.setItem(activeKey(agentAlias), fresh);
    return fresh;
  } catch {
    // Private-mode / quota-exhausted browsers: fall back to a volatile id so
    // chat still works for the lifetime of the page.
    return newSessionId();
  }
}

/** Record `sessionId` as the conversation `agentAlias` is now showing. */
export function setActiveSessionId(agentAlias: string, sessionId: string): void {
  try {
    localStorage.setItem(activeKey(agentAlias), sessionId);
  } catch {
    // Pointer is a convenience; losing it only costs the alias its selection
    // on the next reload.
  }
}
