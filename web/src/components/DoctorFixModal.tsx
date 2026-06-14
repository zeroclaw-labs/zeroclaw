import { useEffect, useId, useRef } from 'react';
import { Link } from 'react-router-dom';
import { ExternalLink, X } from 'lucide-react';
import { Button } from '@/components/ui';
import FieldForm from '@/components/sections/FieldForm';

export interface DoctorFixModalProps {
  /** Whether the modal is mounted/visible. */
  open: boolean;
  /** Dotted config entity prefix to edit, e.g. `providers.models.openai.ss`
   *  or `channels.discord.gnosis`. FieldForm fetches and renders every field
   *  under this prefix and owns its own Save. */
  prefix: string;
  /** Human-friendly entity name shown in the header (e.g. `openai.ss`). */
  entity: string;
  /** Deep-link to the full config page for this entity (the `?tab=…` URL). */
  href: string;
  /** Invoked on Esc, backdrop click, or the close/Done buttons. */
  onClose: () => void;
}

/**
 * Operator Console "fix in place" modal for the Doctor page.
 *
 * Lets the operator edit a flagged config entity's fields inline — without
 * leaving the Doctor list or losing their place — so they don't have to
 * navigate to /config and re-run diagnostics. The entity's editable fields
 * (including the model field) are rendered by the shared `FieldForm`, which
 * also handles Save. A "Open full page →" link is offered for operators who
 * want the complete config surface.
 *
 * Focus-trapped: the close button receives focus on open; Esc and a backdrop
 * click close it; Tab is trapped within the panel; focus is restored to the
 * trigger on close. Mirrors the conventions in `ui/ConfirmDialog`.
 */
export default function DoctorFixModal({
  open,
  prefix,
  entity,
  href,
  onClose,
}: DoctorFixModalProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const titleId = useId();

  // Focus the close button on open; restore focus to the previously-focused
  // element (the trigger) on close.
  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
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
          'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
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
      className="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto py-10"
      onClick={onClose}
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative w-full max-w-2xl mx-4 rounded-[var(--radius-lg)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between gap-3 px-6 pt-5 pb-4 border-b border-pc-border">
          <div className="min-w-0">
            <p className="text-[11px] font-semibold uppercase tracking-wider text-pc-text-muted">
              Fix configuration
            </p>
            <h2
              id={titleId}
              className="text-sm font-semibold text-pc-text font-mono break-all"
            >
              {entity}
            </h2>
          </div>
          <button
            ref={closeRef}
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="inline-flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-[var(--radius-md)] border border-pc-border bg-transparent text-pc-text-secondary transition-colors duration-150 hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Body — the entity's editable fields. FieldForm owns its own Save. */}
        <div className="px-6 py-4 max-h-[60vh] overflow-y-auto">
          <FieldForm prefix={prefix} drift={[]} onSaved={() => undefined} inlineSaveBar />
        </div>

        {/* Footer — escape hatch to the full config page + close. */}
        <div className="flex items-center justify-between gap-2 px-6 py-4 border-t border-pc-border">
          <Link
            to={href}
            className="inline-flex h-9 items-center gap-1.5 rounded-[var(--radius-md)] border border-pc-border bg-transparent px-3.5 text-sm font-medium text-pc-text-secondary transition-colors duration-150 hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
          >
            Open full page
            <ExternalLink className="h-3.5 w-3.5" />
          </Link>
          <Button variant="ghost" onClick={onClose}>
            Done
          </Button>
        </div>
      </div>
    </div>
  );
}
