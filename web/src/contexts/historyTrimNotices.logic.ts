/**
 * History-trim notice rendering, extracted from AgentContext so the web
 * rendering boundary is unit-testable without a React/browser harness.
 *
 * The token-rich notice depends only on `tokens_before`/`tokens_after` being
 * available. The configured budget is optional context — recovery trims toward
 * a provider-overflow target, never the configured limit, so the wording must
 * never present the configured budget as the trim target.
 */

export interface HistoryTrimmedNoticeMessage {
  reason?: string;
  dropped_messages?: number;
  kept_turns?: number;
  token_budget?: number;
  tokens_before?: number;
  tokens_after?: number;
  tokens_before_source?: string;
  tokens_after_source?: string;
}

export function buildHistoryTrimmedNotice(
  msg: HistoryTrimmedNoticeMessage,
  t: (key: string) => string,
): string {
  const reason = msg.reason || t('agent.history_trimmed_unknown_reason');
  const dropped = String(msg.dropped_messages ?? 0);
  const kept = String(msg.kept_turns ?? 0);
  const hasTokens = msg.tokens_before != null && msg.tokens_after != null;
  // The untrimmable newest-turn/schema floor is emitted with `dropped_messages`
  // 0 while the projected `tokens_after` still exceeds the configured budget.
  // It must not claim history was trimmed: render a distinct, truthful notice.
  const atFloor =
    (msg.dropped_messages ?? 0) === 0 &&
    hasTokens &&
    msg.token_budget != null &&
    msg.tokens_after > msg.token_budget;
  if (atFloor) {
    return t('agent.history_trimmed_floor')
      .replace('{reason}', reason)
      .replace('{after}', String(msg.tokens_after))
      .replace('{budget}', String(msg.token_budget));
  }
  if (!hasTokens) {
    return t('agent.history_trimmed')
      .replace('{reason}', reason)
      .replace('{dropped}', dropped)
      .replace('{kept}', kept);
  }
  let content = t('agent.history_trimmed_tokens')
    .replace('{reason}', reason)
    .replace('{before}', String(msg.tokens_before))
    .replace('{after}', String(msg.tokens_after))
    .replace('{dropped}', dropped)
    .replace('{kept}', kept);
  if (msg.token_budget != null) {
    content += t('agent.history_trimmed_tokens_budget_clause').replace(
      '{budget}',
      String(msg.token_budget),
    );
  }
  const sourceLabel = (source?: string) =>
    source === 'provider'
      ? t('agent.history_trimmed_tokens_source_provider')
      : source === 'estimate'
        ? t('agent.history_trimmed_tokens_source_estimate')
        : source === 'calibrated'
          ? t('agent.history_trimmed_tokens_source_calibrated')
          : undefined;
  const beforeLabel = sourceLabel(msg.tokens_before_source);
  const afterLabel = sourceLabel(msg.tokens_after_source);
  if (beforeLabel && afterLabel) {
    content +=
      ' ' +
      t('agent.history_trimmed_tokens_sources')
        .replace('{before}', beforeLabel)
        .replace('{after}', afterLabel);
  }
  return content;
}
