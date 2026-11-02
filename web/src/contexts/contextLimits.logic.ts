import type { WsMessage } from '../types/api';

export interface ContextLimitsState {
  maxTokens: number | null;
  modelWindow: number | null;
}

export const EMPTY_CONTEXT_LIMITS: ContextLimitsState = {
  maxTokens: null,
  modelWindow: null,
};

/** Read a done frame as one authoritative budget/capacity snapshot. */
export function contextLimitsFromDoneFrame(
  frame: Pick<WsMessage, 'max_context_tokens' | 'model_context_window'>,
): ContextLimitsState {
  return {
    maxTokens: typeof frame.max_context_tokens === 'number'
      ? frame.max_context_tokens
      : null,
    modelWindow: typeof frame.model_context_window === 'number'
      ? frame.model_context_window
      : null,
  };
}

export interface DisplayedContextBudget {
  inputTokens: number | null;
  maxTokens: number | null;
}

/**
 * Apply a history-trim frame to the displayed context budget.
 *
 * A real trim supplies a new retained-context estimate. Its token_budget can
 * be an emergency dispatch threshold, not the canonical configured budget.
 * Zero-drop floor notices and message-only trims leave the count alone; all
 * budget/capacity updates remain owned by the accepted done-frame snapshot.
 */
export function applyHistoryTrimToDisplayedBudget(
  current: DisplayedContextBudget,
  frame: Pick<WsMessage, 'dropped_messages' | 'tokens_after'>,
): DisplayedContextBudget {
  if (!(typeof frame.dropped_messages === 'number' && frame.dropped_messages > 0)
    || typeof frame.tokens_after !== 'number') return current;
  return {
    inputTokens: frame.tokens_after,
    maxTokens: current.maxTokens,
  };
}
