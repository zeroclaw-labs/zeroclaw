import { useEffect, useRef, useState } from 'react';
import { X } from 'lucide-react';
import { Button } from '@/components/ui';
import { useFocusTrap } from '@/hooks/useFocusTrap';
import { t } from '@/lib/i18n';

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

  // Esc closes; Tab is trapped inside the dialog panel; focus is restored to the
  // trigger on close. Matches the prior hand-rolled effect (default selector —
  // no select/textarea, no visibility filter, Esc does not preventDefault).
  // The dialog is mounted only while open, so the trap is always enabled.
  // Declared before the focus-on-open effect so the trap captures the trigger as
  // the restore target before focus moves into the input.
  useFocusTrap(panelRef, { onClose: onCancel });

  // Focus + select the input on open. (Store/restore + Esc/Tab handled above.)
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const confirm = () => {
    const trimmed = value.trim();
    onConfirm(trimmed || suggestion);
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={`${t('alias_prompt.name_this_prefix')}${label}${t('alias_prompt.name_this_suffix')}`}
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
            {t('alias_prompt.name_this_prefix')}{label}{t('alias_prompt.name_this_suffix')}
          </h2>
          <button
            type="button"
            onClick={onCancel}
            aria-label={t('common.close')}
            className="h-8 w-8 rounded-[var(--radius-md)] flex items-center justify-center text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-5 flex flex-col gap-3">
          <p className="text-xs text-pc-text-muted">
            {t('alias_prompt.description_prefix')}{' '}
            <span className="text-pc-text-secondary">{t('alias_prompt.example_work')}</span>,{' '}
            <span className="text-pc-text-secondary">{t('alias_prompt.example_personal')}</span>,{' '}
            {t('alias_prompt.or')}{' '}
            <span className="text-pc-text-secondary">{t('alias_prompt.example_default')}</span>
            {t('alias_prompt.description_suffix')}
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
            {t('common.cancel')}
          </Button>
          <Button variant="primary" onClick={confirm}>
            {t('common.confirm')}
          </Button>
        </div>
      </div>
    </div>
  );
}
