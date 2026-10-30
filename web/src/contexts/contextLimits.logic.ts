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
 * A trim is emitted after the usage sample that triggered it, and the web
 * meter otherwise refreshes only on the terminal `done` frame. When the trim
 * carries a post-trim count, that count replaces the displayed numerator and
 * the budget that governed the trim replaces the displayed trim budget.
 * Message-limit trims carry no token accounting and must leave the displayed
 * snapshot alone. Model capacity is not on this frame, so it is not touched.
 */
export function applyHistoryTrimToDisplayedBudget(
  current: DisplayedContextBudget,
  frame: Pick<WsMessage, 'tokens_after' | 'token_budget'>,
): DisplayedContextBudget {
  if (typeof frame.tokens_after !== 'number') return current;
  return {
    inputTokens: frame.tokens_after,
    maxTokens: typeof frame.token_budget === 'number' ? frame.token_budget : current.maxTokens,
  };
}
