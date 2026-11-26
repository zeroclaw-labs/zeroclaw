import type { ChatMessage } from '@/contexts/AgentContext';

// Channel-user (and some agent) messages arrive with a leading
// `[YYYY-MM-DD HH:MM:SS TZ] ` prefix the gateway prepends. The zone is a chrono
// `%Z` abbreviation (e.g. CEST) that JS `Date` can't reliably parse, so we
// don't try — we just strip the prefix for display and copy; the bubble shows
// its own wall-clock caption separately. Anchored to the start so a bracketed
// datetime appearing mid-message (a log line, an error report) is left intact.
const SERVER_TIMESTAMP_RE = /^\s*\[\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [^\]]+\]\s*/;


export function displayMessageText(message: Pick<ChatMessage, 'content' | 'local' | 'ephemeral'>): string {
  return message.local || message.ephemeral ? message.content : message.content.replace(SERVER_TIMESTAMP_RE, '');
}
