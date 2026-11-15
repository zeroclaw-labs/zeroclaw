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

/** Count and TokenCountSource wire provenance are one display snapshot. */
export interface DisplayedContextInput {
  inputTokens: number | null;
  source: string | null;
}

export const EMPTY_CONTEXT_INPUT: DisplayedContextInput = {
  inputTokens: null,
  source: null,
};

/** A real trim updates only its retained-context count and provenance.
 * The event threshold is not a canonical configured limit. Zero-drop floor
 * notices and message-only trims leave the display snapshot untouched.
 */
export function applyHistoryTrimToDisplayedBudget(
  current: DisplayedContextInput,
  frame: Pick<WsMessage, 'dropped_messages' | 'tokens_after' | 'tokens_after_source'>,
): DisplayedContextInput {
  if (!(typeof frame.dropped_messages === 'number' && frame.dropped_messages > 0)
    || typeof frame.tokens_after !== 'number') return current;
  return { inputTokens: frame.tokens_after, source: frame.tokens_after_source ?? null };
}

/** Prefer a display-only trim snapshot without reinterpreting measured usage.
 * Explicit null clears stale accepted-route usage; absent legacy fields do not.
 */
export function contextInputFromDoneFrame(
  current: DisplayedContextInput,
  frame: Pick<WsMessage, 'context_input_tokens' | 'context_input_tokens_source' | 'last_input_tokens' | 'input_tokens'>,
): DisplayedContextInput {
  if (typeof frame.context_input_tokens === 'number') {
    return { inputTokens: frame.context_input_tokens, source: frame.context_input_tokens_source ?? null };
  }
  if (frame.last_input_tokens === null) return EMPTY_CONTEXT_INPUT;
  if (typeof frame.last_input_tokens === 'number') {
    return { inputTokens: frame.last_input_tokens, source: 'provider' };
  }
  if (typeof frame.input_tokens === 'number') {
    return { inputTokens: frame.input_tokens, source: 'provider' };
  }
  return current;
}
