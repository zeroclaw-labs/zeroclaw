import type { ChatMessage } from '@/contexts/AgentContext';
import { normalizeServerUserContent } from '@/contexts/chatHydration.logic';

// Channel-user (and some agent) messages arrive with a leading
// `[YYYY-MM-DD HH:MM:SS TZ] ` prefix the gateway prepends. The zone is a chrono
// `%Z` abbreviation (e.g. CEST) that JS `Date` can't reliably parse, so we
// don't try — we just strip the prefix for display and copy; the bubble shows
// its own wall-clock caption separately. Anchored to the start so a bracketed
// datetime appearing mid-message (a log line, an error report) is left intact.
const SERVER_TIMESTAMP_RE = /^\s*\[\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [^\]]+\]\s*/;

// A server user turn from the agent path carries the runtime's
// `[CURRENT DATE & TIME: …]` prefix instead (see chatHydration.logic). Strip
// one prefix or the other, never both, so a bracketed datetime the user typed
// at the start of their message survives. Locally composed input and command
// output are verbatim and never stripped.
export function displayMessageText(
  message: Pick<ChatMessage, 'role' | 'content' | 'local' | 'ephemeral'>,
): string {
  if (message.local || message.ephemeral) return message.content;
  if (message.role === 'user') {
    const unlabeled = normalizeServerUserContent(message.content);
    if (unlabeled !== message.content) return unlabeled;
  }
  return message.content.replace(SERVER_TIMESTAMP_RE, '');
}
