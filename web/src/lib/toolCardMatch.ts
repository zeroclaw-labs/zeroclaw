interface ToolCardLike {
  toolCall?: { output?: string; id?: string };
}

/** Resolve which pending tool card a `tool_result` frame belongs to.
 *
 * Parallel tool batches can complete out of call order, so a result must be
 * correlated to its card by the gateway `tool_call_id` rather than by position.
 * When the frame carries no id (legacy/non-gateway paths) or no id-keyed
 * pending card exists, fall back to the first unresolved card so id-less
 * streams still resolve. Returns -1 when nothing is pending. */
export function resolveToolResultIndex<T extends ToolCardLike>(
  messages: readonly T[],
  resultId: string | undefined,
): number {
  const firstUnresolved = () =>
    messages.findIndex((m) => m.toolCall && m.toolCall.output === undefined);
  if (!resultId) return firstUnresolved();
  const byId = messages.findIndex(
    (m) => m.toolCall && m.toolCall.output === undefined && m.toolCall.id === resultId,
  );
  return byId === -1 ? firstUnresolved() : byId;
}
