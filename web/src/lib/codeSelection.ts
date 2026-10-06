import type { Session } from '@/types/api';

// A browser preference creates the fact of which conversation this operator
// last selected. Session existence, ownership and status come from the daemon.
const KEY = 'zeroclaw-code-selection';
export function loadCodeSelection(): { agent: string; session: string } | null {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(KEY) ?? 'null');
    if (
      value &&
      typeof value === 'object' &&
      'agent' in value &&
      'session' in value &&
      typeof value.agent === 'string' &&
      typeof value.session === 'string'
    )
      return value as { agent: string; session: string };
  } catch {
    /* Storage may be disabled. The daemon list is the fallback. */
  }
  return null;
}
export function saveCodeSelection(agent: string, session: string) {
  try {
    localStorage.setItem(KEY, JSON.stringify({ agent, session }));
  } catch {
    /* Optional browser preference. */
  }
}
export function resumeCodeSession(
  history: (Session & { interaction_surface?: string })[],
  agents: string[],
  selection = loadCodeSelection(),
) {
  const valid = history.filter(
    (item) =>
      item.interaction_surface === 'zerocode_code' &&
      agents.includes(item.agent_alias ?? ''),
  );
  return (
    valid.find(
      (item) =>
        item.session_id === selection?.session &&
        item.agent_alias === selection.agent,
    ) ??
    [...valid].sort((a, b) => b.last_activity.localeCompare(a.last_activity))[0]
  );
}
