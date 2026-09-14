/** Terminal-explanation arbitration for a single turn.
 *
 *  A turn that dies on unrecoverable context exhaustion produces two gateway
 *  frames: the `context_exhausted` notice (localized, explains *why* the turn
 *  stopped) and the generic `error` frame that always follows a failed turn.
 *  Rendering both leaves the transcript restating one stop twice — once in
 *  operator wording, once in raw provider wording.
 *
 *  This module owns that decision so it is testable at the frame boundary
 *  rather than living as untestable ref mutations inside the context provider.
 *  See #8758.
 */

/** Frames that can change which terminal explanation a turn renders. */
export type TerminalFrame =
  | { type: 'turn_start' }
  | { type: 'context_exhausted'; notice?: string }
  | { type: 'error' };

export interface TerminalExplanationState {
  /** A context-exhaustion notice already explained the in-flight turn. */
  explained: boolean;
}

/** What the handler should render for this frame. */
export type TerminalRender =
  | { kind: 'none' }
  /** The localized context-exhaustion notice bubble. */
  | { kind: 'notice'; content: string }
  /** The generic error bubble. */
  | { kind: 'error' };

export interface TerminalFrameOutcome {
  state: TerminalExplanationState;
  render: TerminalRender;
  /** The frame starts or supplies a new terminal explanation, so a banner
   *  inherited from an older turn is stale. */
  clearBanner: boolean;
}

export function contextExhaustedBubblePresentation(persisted?: boolean) {
  return {
    markdown: true,
    ephemeral: persisted !== false,
  };
}

export function initialTerminalExplanationState(): TerminalExplanationState {
  return { explained: false };
}

/** Which status banner an `error` frame should raise, if any. */
export type TerminalBanner =
  | { kind: 'none' }
  | { kind: 'configuration' }
  | { kind: 'message' };

/**
 * Decide the banner for an `error` frame, given the arbitration outcome.
 *
 * Split out of the context provider because the banner is a *second* surface
 * that must honour the same verdict as the bubble. Context exhaustion arrives
 * as `PROVIDER_ERROR`, so keying the banner off `code` alone re-surfaced the
 * raw provider text under a "Configuration error" heading even while the
 * bubble was correctly suppressed — misleading, since nothing is
 * misconfigured. Pure so it is testable without a DOM harness (#8758).
 */
export function bannerForErrorFrame(render: TerminalRender, code?: string): TerminalBanner {
  // An explained turn renders no banner: the notice is the whole explanation.
  if (render.kind !== 'error') return { kind: 'none' };
  if (code === 'AGENT_INIT_FAILED' || code === 'AUTH_ERROR' || code === 'PROVIDER_ERROR') {
    return { kind: 'configuration' };
  }
  if (code === 'INVALID_JSON' || code === 'UNKNOWN_MESSAGE_TYPE' || code === 'EMPTY_CONTENT') {
    return { kind: 'message' };
  }
  return { kind: 'none' };
}

export function reduceTerminalFrame(
  state: TerminalExplanationState,
  frame: TerminalFrame,
): TerminalFrameOutcome {
  switch (frame.type) {
    case 'turn_start':
      // A new turn must never inherit the previous turn's explanation, or an
      // unrelated later failure would render silently.
      return { state: { explained: false }, render: { kind: 'none' }, clearBanner: true };
    case 'context_exhausted':
      // A frame with no notice text carries nothing to render; leave the turn
      // unexplained so the error frame still surfaces the failure.
      if (!frame.notice) return { state, render: { kind: 'none' }, clearBanner: false };
      return {
        state: { explained: true },
        render: { kind: 'notice', content: frame.notice },
        clearBanner: true,
      };
    case 'error':
      if (state.explained) {
        // Consume the explanation: this turn is over.
        return { state: { explained: false }, render: { kind: 'none' }, clearBanner: false };
      }
      return { state, render: { kind: 'error' }, clearBanner: false };
  }
}
