import { useEffect, useRef, type RefObject } from 'react';

/**
 * The focusable-element selector most of the console's modals share. Components
 * that allow `<select>` / `<textarea>` controls inside the trap pass the wider
 * `FOCUSABLE_SELECTOR_FORM` variant instead.
 */
export const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Wider selector for dialogs that also contain `<select>` / `<textarea>`. */
export const FOCUSABLE_SELECTOR_FORM =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export interface UseFocusTrapOptions {
  /** Invoked when Escape is pressed while the trap is active. */
  onClose: () => void;
  /**
   * When false the trap is inert (no listener, no focus capture/restore).
   * Defaults to true. Components that only mount their dialog while open can
   * leave this unset.
   */
  enabled?: boolean;
  /**
   * Focusable-element selector used to find the first/last tab stops. Defaults
   * to {@link FOCUSABLE_SELECTOR}; pass {@link FOCUSABLE_SELECTOR_FORM} (or a
   * custom string) for dialogs containing `<select>` / `<textarea>`.
   */
  focusableSelector?: string;
  /**
   * When true, only visible elements (`offsetParent !== null`, plus the active
   * element) are considered tab stops — matches the dialogs that hide collapsed
   * sub-content. Defaults to false (every selector match counts).
   */
  filterVisible?: boolean;
  /**
   * When true, `preventDefault()` is called on the Escape keydown before
   * `onClose`. Defaults to false (matches dialogs that let Escape bubble).
   */
  preventDefaultOnEscape?: boolean;
}

/**
 * Shared modal focus trap, extracted from the near-identical hand-rolled effects
 * in AgentDrawer / AliasPromptDialog / DoctorFixModal (and mirrored by
 * ui/ConfirmDialog). While `enabled`:
 *
 * - remembers `document.activeElement` on enable and restores focus to it on
 *   disable/unmount (the trigger that opened the dialog);
 * - calls `onClose` on Escape (optionally `preventDefault`-ing first);
 * - wraps Tab / Shift+Tab within the focusable elements inside `ref`.
 *
 * It deliberately does NOT move initial focus into the dialog — each consumer
 * focuses its own preferred element (close button, search input, …) in a
 * separate effect, so that behavior is preserved exactly.
 *
 * The handler is registered on `window` (matching the original effects), so the
 * trap fires regardless of where focus currently sits within the dialog.
 */
export function useFocusTrap(
  ref: RefObject<HTMLElement | null>,
  {
    onClose,
    enabled = true,
    focusableSelector = FOCUSABLE_SELECTOR,
    filterVisible = false,
    preventDefaultOnEscape = false,
  }: UseFocusTrapOptions,
): void {
  // Keep the latest onClose without re-arming the listener effect on each render.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  // Remember the element focused when the trap turned on; restore it on cleanup.
  useEffect(() => {
    if (!enabled) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    return () => previouslyFocused?.focus?.();
  }, [enabled]);

  // Esc closes; Tab / Shift+Tab wrap within the dialog's focusable elements.
  useEffect(() => {
    if (!enabled) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (preventDefaultOnEscape) e.preventDefault();
        onCloseRef.current();
        return;
      }
      if (e.key !== 'Tab') return;
      const panel = ref.current;
      if (!panel) return;
      let focusable = Array.from(panel.querySelectorAll<HTMLElement>(focusableSelector));
      if (filterVisible) {
        focusable = focusable.filter(
          (el) => el.offsetParent !== null || el === document.activeElement,
        );
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      const active = document.activeElement;
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [enabled, ref, focusableSelector, filterVisible, preventDefaultOnEscape]);
}

export default useFocusTrap;
