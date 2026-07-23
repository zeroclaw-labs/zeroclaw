// Pure completion/turn-stream reducer used by AgentContext's WebSocket handler
// so the production lifecycle is testable without mounting React. This file
// makes no DOM/i18n/UUID side effects; it owns only per-turn stream state and
// completion classification.

/** Accumulated per-turn streaming state the reducer folds frames into. */
export interface TurnStreamState {
  /** Content deltas concatenated so far this turn (`chunk` frames). */
  pendingContent: string;
  /** Reasoning deltas concatenated so far this turn (`thinking` frames). */
  pendingThinking: string;
  /** Thinking snapshot taken at `chunk_reset`, before display state cleared. */
  capturedThinking: string;
  /** Whether a real `tool_call` (with a name) arrived this turn. */
  hadToolCall: boolean;
}

/** Fresh per-turn state. Returned after every completion so the next turn
 *  starts clean — the "ref state resets across turns" invariant. */
export function initialTurnStreamState(): TurnStreamState {
  return {
    pendingContent: '',
    pendingThinking: '',
    capturedThinking: '',
    hadToolCall: false,
  };
}

/** Events that affect the current turn's accumulated stream state. The handler
 *  maps gateway frames and local lifecycle boundaries to these before folding;
 *  events the completion decision does not depend on are not modeled. */
export type TurnStreamFrame =
  | { type: 'thinking'; content?: string }
  | { type: 'chunk'; content?: string }
  | { type: 'chunk_reset' }
  // `hasName` mirrors the handler's `if (!msg.name) break` guard: a nameless
  // observability telemetry frame is not a real tool call and must not flip
  // `hadToolCall` (issue #7151).
  | { type: 'tool_call'; hasName: boolean }
  | { type: 'done' | 'message'; full_response?: string; content?: string }
  // Every terminal/non-completion boundary resets the same canonical state.
  // `turn_start` is emitted locally after a new message is sent.
  | { type: 'turn_start' | 'aborted' | 'error' | 'reset' };

/** What the handler should do with the finished turn. `commit` renders an
 *  assistant bubble (possibly empty content when reasoning is present);
 *  `diagnostic` renders the one-off no-output notice; `skip` renders nothing
 *  (the tool cards are already the visible record — #6702). */
export type CompletionOutcome =
  | { kind: 'commit'; content: string; thinking?: string }
  | { kind: 'diagnostic' }
  | { kind: 'skip' };

/** Classify a finished turn from its accumulated state and the terminal frame.
 *  Pure: the whole reasoning-only / empty / tool-only decision lives here. */
export function classifyCompletion(
  state: TurnStreamState,
  frame: { full_response?: string; content?: string },
): CompletionOutcome {
  // Fallback chain matches the handler: an explicit `full_response`, then a
  // frame `content`, then whatever was streamed live.
  const raw = frame.full_response ?? frame.content ?? state.pendingContent;
  // Trim so whitespace-only content (models that emit "\n\n" alongside
  // tool_calls) does not create a blank bubble (#6702).
  const content = raw.trim();
  const thinking =
    state.capturedThinking || state.pendingThinking || undefined;

  if (content || thinking) {
    // Reasoning-only turns land here with empty content but present thinking,
    // so the turn is visible instead of vanishing silently.
    return { kind: 'commit', content, thinking };
  }
  if (!state.hadToolCall) {
    // Nothing at all on a clean completion — surface a diagnostic so the turn
    // does not disappear. Mirrors zerocode's zc-turn-no-output (#8779).
    return { kind: 'diagnostic' };
  }
  // Empty content but tools ran: their cards are the record, render nothing.
  return { kind: 'skip' };
}

/** Fold one frame into the turn state. For `done`/`message` the returned
 *  `completion` is the classification and the returned `state` is fresh (the
 *  turn is over); for every other frame `completion` is null and `state`
 *  carries the accumulation forward. */
export function reduceTurnFrame(
  state: TurnStreamState,
  frame: TurnStreamFrame,
): { state: TurnStreamState; completion: CompletionOutcome | null } {
  switch (frame.type) {
    case 'thinking':
      return {
        state: { ...state, pendingThinking: state.pendingThinking + (frame.content ?? '') },
        completion: null,
      };
    case 'chunk':
      return {
        state: { ...state, pendingContent: state.pendingContent + (frame.content ?? '') },
        completion: null,
      };
    case 'chunk_reset':
      // Snapshot thinking, then clear the live display buffers. The server
      // signals the authoritative done message follows.
      return {
        state: {
          ...state,
          capturedThinking: state.pendingThinking,
          pendingContent: '',
          pendingThinking: '',
        },
        completion: null,
      };
    case 'tool_call':
      if (!frame.hasName) return { state, completion: null };
      return { state: { ...state, hadToolCall: true }, completion: null };
    case 'done':
    case 'message': {
      const completion = classifyCompletion(state, frame);
      // Turn is over: hand back fresh state so the next turn starts clean.
      return { state: initialTurnStreamState(), completion };
    }
    case 'turn_start':
    case 'aborted':
    case 'error':
    case 'reset':
      return { state: initialTurnStreamState(), completion: null };
  }
}
