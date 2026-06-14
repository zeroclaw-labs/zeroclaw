import { useEffect, useId, useRef, type ReactNode } from 'react';
import { Button } from '@/components/ui/Button';

export interface ConfirmDialogProps {
  /** Whether the dialog is mounted/visible. */
  open: boolean;
  /** Dialog heading. */
  title: string;
  /** Optional supporting copy (string or nodes). */
  message?: ReactNode;
  /** Label for the confirm button. Defaults to "Confirm". */
  confirmLabel?: string;
  /** Label for the cancel button. Defaults to "Cancel". */
  cancelLabel?: string;
  /** Render the confirm action as destructive (danger variant). */
  danger?: boolean;
  /** Invoked when the user confirms. */
  onConfirm: () => void;
  /** Invoked on cancel, Esc, or backdrop click. */
  onClose: () => void;
}

/**
 * Operator Console confirmation modal.
 *
 * A centered, focus-trapped replacement for `window.confirm`. The confirm/cancel
 * buttons receive focus on open; Esc and a backdrop click close it; Tab is
 * trapped within the panel and focus is restored to the trigger on close.
 *
 * Mirrors the modal conventions in `SettingsModal`/`AliasPromptDialog` and works
 * in both operator-dark and operator-light via the token classes.
 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel = 'Confirm',
  cancelLabel = 'Cancel',
  danger = false,
  onConfirm,
  onClose,
}: ConfirmDialogProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descId = useId();

  // Focus the confirm button on open; restore focus to the previously-focused
  // element (the trigger) on close. The confirm button is the last focusable
  // control in the panel, so we target it via the panel rather than a ref —
  // the shared `Button` primitive does not forward refs.
  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    const buttons = panelRef.current?.querySelectorAll<HTMLButtonElement>('button');
    buttons?.[buttons.length - 1]?.focus();
    return () => previouslyFocused?.focus?.();
  }, [open]);

  // Esc closes; Tab is trapped inside the dialog panel.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key !== 'Tab') return;
      const panel = panelRef.current;
      if (!panel) return;
      const focusable = Array.from(
        panel.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((el) => el.offsetParent !== null || el === document.activeElement);
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
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={message != null ? descId : undefined}
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={onClose}
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative w-full max-w-sm mx-4 rounded-[var(--radius-xl)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Body */}
        <div className="px-6 pt-5 pb-4 flex flex-col gap-2">
          <h2 id={titleId} className="text-sm font-semibold text-pc-text">
            {title}
          </h2>
          {message != null && (
            <div id={descId} className="text-xs leading-relaxed text-pc-text-muted">
              {message}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 px-6 py-4 border-t border-pc-border">
          <Button variant="ghost" onClick={onClose}>
            {cancelLabel}
          </Button>
          <Button
            variant={danger ? 'danger' : 'primary'}
            onClick={onConfirm}
          >
            {confirmLabel}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default ConfirmDialog;
