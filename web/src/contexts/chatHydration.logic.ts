// Pure helpers for chat hydration. No imports: the logic suite runs under
// `node --experimental-strip-types --test` without the web alias config.

/** The shape of a transcript bubble that these helpers read. */
export interface HydrationBubble {
  role: 'user' | 'agent';
  content: string;
  local?: boolean;
  toolCall?: unknown;
}

// The runtime prefixes every user turn it persists with
// `[CURRENT DATE & TIME: <date> <tz>]\n\n` (agent.rs `enrich_user_message`).
const ENRICHMENT_PREFIX_RE = /^\s*\[CURRENT DATE & TIME: [^\]]*\]\s*/;

/** A persisted user row's content with the runtime's date prefix removed. */
export function normalizeServerUserContent(content: string): string {
  return content.replace(ENRICHMENT_PREFIX_RE, '');
}

// What a user bubble says, for comparing it with the server's rows. A bubble
// the browser composed (`local: true`) is the user's verbatim input and never
// carries the gateway's prefix, so it is compared as is: stripping it would
// also strip a prefix the user typed themselves. Every other row came from the
// server and carries the gateway's prefix.
function comparableContent(bubble: HydrationBubble): string {
  return bubble.local === true ? bubble.content : normalizeServerUserContent(bubble.content);
}

// The last bubble of a finished turn: an agent reply that is not a tool call.
function isFinalAnswer(bubble: HydrationBubble): boolean {
  return bubble.role === 'agent' && !bubble.toolCall;
}

/**
 * The tail of the local transcript that a server snapshot cannot contain yet.
 *
 * The gateway commits a turn's user prompt only when the turn finishes, so a
 * snapshot fetched while a detached turn is still running predates the prompt
 * the browser already showed. The tail starts at the earliest pending prompt
 * the browser composed itself (`local: true`) and runs to the end of the local
 * transcript.
 *
 * A prompt is pending when it has more occurrences in the local transcript, up
 * to and including itself, than in the snapshot. Counting occurrences, rather
 * than testing presence, keeps a repeated prompt ("continue") from being
 * mistaken for one the snapshot already holds, and the gateway commits turns in
 * order, so the committed occurrences are always the earliest ones.
 *
 * Starting from the last local prompt, earlier pending prompts are included
 * while no finished turn lies between them: a turn that already has its final
 * answer in the local copy is not pending, and neither is any prompt the
 * snapshot holds. That keeps a second prompt, queued behind a turn that is
 * still running, from displacing the first one, without bringing back turns the
 * server has finished with.
 */
export function uncommittedLocalTail<T extends HydrationBubble>(
  snapshotUserContents: readonly string[],
  local: readonly T[],
): T[] {
  const inSnapshot = new Map<string, number>();
  for (const content of snapshotUserContents) {
    const key = normalizeServerUserContent(content);
    inSnapshot.set(key, (inSnapshot.get(key) ?? 0) + 1);
  }

  const occurrences = new Map<string, number>();
  const committed: boolean[] = new Array<boolean>(local.length).fill(false);
  local.forEach((bubble, index) => {
    if (bubble.role !== 'user') return;
    const key = comparableContent(bubble);
    const rank = (occurrences.get(key) ?? 0) + 1;
    occurrences.set(key, rank);
    committed[index] = rank <= (inSnapshot.get(key) ?? 0);
  });

  let last = -1;
  for (let i = local.length - 1; i >= 0; i -= 1) {
    if (local[i]!.role === 'user' && local[i]!.local === true) {
      last = i;
      break;
    }
  }
  if (last < 0 || committed[last]) return [];

  let start = last;
  for (let i = last - 1; i >= 0; i -= 1) {
    const bubble = local[i]!;
    if (isFinalAnswer(bubble)) break;
    if (bubble.role === 'user') {
      if (bubble.local === true && !committed[i]) start = i;
      else break;
    }
  }
  return local.slice(start);
}
