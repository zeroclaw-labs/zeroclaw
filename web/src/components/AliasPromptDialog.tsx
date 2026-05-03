import { useEffect, useRef, useState } from 'react';
import { X } from 'lucide-react';

interface Props {
  label: string;
  suggestion: string;
  onConfirm: (alias: string) => void;
  onCancel: () => void;
}

export default function AliasPromptDialog({ label, suggestion, onConfirm, onCancel }: Props) {
  const [value, setValue] = useState(suggestion);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel();
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
      <div
        className="absolute inset-0"
        style={{ background: 'rgba(0,0,0,0.6)', backdropFilter: 'blur(8px)' }}
      />
      <div
        className="relative w-full max-w-sm mx-4 rounded-3xl border shadow-2xl animate-fade-in"
        style={{ background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div
          className="flex items-center justify-between px-6 py-4 border-b"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          <h2 className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            Name this {label} configuration
          </h2>
          <button
            type="button"
            onClick={onCancel}
            className="h-8 w-8 rounded-xl flex items-center justify-center transition-colors"
            style={{ color: 'var(--pc-text-muted)', background: 'transparent', border: 'none', cursor: 'pointer' }}
            onMouseEnter={(e) => (e.currentTarget.style.background = 'var(--pc-bg-elevated)')}
            onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-5 flex flex-col gap-3">
          <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
            Choose an alias for this entry — e.g. <span style={{ color: 'var(--pc-text-secondary)' }}>"work"</span>,{' '}
            <span style={{ color: 'var(--pc-text-secondary)' }}>"personal"</span>, or{' '}
            <span style={{ color: 'var(--pc-text-secondary)' }}>"default"</span>. Multiple configurations of the same
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
        <div
          className="flex items-center justify-end gap-2 px-6 py-4 border-t"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          <button
            type="button"
            onClick={onCancel}
            className="btn-secondary text-sm px-4 py-2"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={confirm}
            className="btn-electric text-sm px-4 py-2"
          >
            Confirm
          </button>
        </div>
      </div>
    </div>
  );
}
