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
