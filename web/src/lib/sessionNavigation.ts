import type { Session } from "../types/api";

/** History opens read-only first; reconnecting a running session can fork its agent. */
export function sessionTarget(session: Session & { surface?: "code" }): string {
  if (session.surface === "code")
    return `/code?agent=${encodeURIComponent(session.agent_alias ?? "")}&session=${encodeURIComponent(session.session_id)}`;
  return `/sessions?session=${encodeURIComponent(session.session_key)}`;
}

export function resumeSessionTarget(session: Session): string | null {
  if (!session.session_key.startsWith("gw_") || !session.agent_alias)
    return null;
  return `/agent/${encodeURIComponent(session.agent_alias)}?session=${encodeURIComponent(session.session_key.slice(3))}`;
}
