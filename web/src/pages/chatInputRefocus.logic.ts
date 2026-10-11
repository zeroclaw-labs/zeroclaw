/**
 * Decides whether the chat message box should take focus back when a turn
 * ends. The textarea is disabled for the whole turn, which drops the focus
 * `handleSend` gave it, so nothing would otherwise return it.
 *
 * - `arm`: a turn is running; remember to refocus when it ends.
 * - `wait`: the turn ended but the box is still disabled (disconnected or not
 *   hydrated); stay armed until it is enabled.
 * - `focus`: focus the box.
 * - `skip`: the turn ended in a hidden pane, or the user moved focus to another
 *   control during the turn; disarm without taking focus.
 * - `none`: nothing to do.
 */
export type ChatInputRefocusAction = 'arm' | 'wait' | 'focus' | 'skip' | 'none';

/** Where the document focus is relative to this pane's message box. */
export type ChatInputFocusOwner = 'nothing' | 'input' | 'other';

export interface ChatInputRefocusState {
  /** A turn started since the last refocus decision. */
  armed: boolean;
  typing: boolean;
  inputEnabled: boolean;
  /** The pane is rendered (background chat tabs are `display: none`). */
  inputVisible: boolean;
  focusOwner: ChatInputFocusOwner;
}

export function nextChatInputRefocus(state: ChatInputRefocusState): ChatInputRefocusAction {
  if (state.typing) return 'arm';
  if (!state.armed) return 'none';
  if (!state.inputEnabled) return 'wait';
  if (!state.inputVisible || state.focusOwner === 'other') return 'skip';
  return 'focus';
}

/**
 * Classifies `document.activeElement`. Browsers move focus off a control that
 * becomes disabled either to the body or (Firefox) leave it on the disabled
 * control, so both count as "the user has not focused anything else".
 */
export function chatInputFocusOwner(
  activeElement: unknown,
  body: unknown,
  input: unknown,
): ChatInputFocusOwner {
  if (activeElement === input) return 'input';
  if (activeElement == null || activeElement === body) return 'nothing';
  return 'other';
}
