import { useEffect, useRef, useState } from 'react';
import { X } from 'lucide-react';
import { Button } from '@/components/ui';

interface Props {
  label: string;
  suggestion: string;
  onConfirm: (alias: string) => void;
  onCancel: () => void;
}

export default function AliasPromptDialog({ label, suggestion, onConfirm, onCancel }: Props) {
  const [value, setValue] = useState(suggestion);
  const inputRef = useRef<HTMLInputElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // Focus the input on open; restore focus to the previously-focused element
  // (the trigger) on close.
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    inputRef.current?.select();
    return () => previouslyFocused?.focus?.();
  }, []);

  // Esc closes; Tab is trapped inside the dialog panel.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onCancel();
        return;
      }
      if (e.key !== 'Tab') return;
      const panel = panelRef.current;
      if (!panel) return;
      const focusable = panel.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
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
  }, [onCancel]);

  const confirm = () => {
    const trimmed = value.trim();
    onConfirm(trimmed || suggestion);
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={`Name this ${label} configuration`}
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={onCancel}
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative w-full max-w-sm mx-4 rounded-[var(--radius-xl)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-pc-border">
          <h2 className="text-sm font-semibold text-pc-text">
            Name this {label} configuration
          </h2>
          <button
            type="button"
            onClick={onCancel}
            aria-label="Close"
            className="h-8 w-8 rounded-[var(--radius-md)] flex items-center justify-center text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-5 flex flex-col gap-3">
          <p className="text-xs text-pc-text-muted">
            Choose an alias for this entry — e.g. <span className="text-pc-text-secondary">"work"</span>,{' '}
            <span className="text-pc-text-secondary">"personal"</span>, or{' '}
            <span className="text-pc-text-secondary">"default"</span>. Multiple configurations of the same
            type can coexist under different aliases.
          </p>
          <input
            ref={inputRef}
            type="text"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') confirm();
            }}
            placeholder={suggestion}
            className="input-electric w-full px-3 py-2 text-sm"
          />
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 px-6 py-4 border-t border-pc-border">
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="primary" onClick={confirm}>
            Confirm
          </Button>
        </div>
      </div>
    </div>
  );
}
