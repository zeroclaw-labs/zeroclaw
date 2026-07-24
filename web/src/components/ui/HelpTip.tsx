import { useState, type ReactNode } from 'react';
import { Info } from 'lucide-react';

/// Small info affordance that reveals help text on hover or focus. Text comes
/// from the backend-generated field descriptions (Rust `///` docs); this only
/// renders whatever string it is handed.
export function HelpTip({ text, children }: { text?: string | null; children?: ReactNode }) {
  const [open, setOpen] = useState(false);
  if (!text) return children ? <>{children}</> : null;
  return (
    <span className="relative inline-flex items-center gap-1">
      {children}
      <span
        className="inline-flex cursor-help text-pc-text-faint hover:text-pc-text-muted"
        tabIndex={0}
        role="button"
        aria-label={text}
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={() => setOpen(false)}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
      >
        <Info size={13} aria-hidden />
      </span>
      {open ? (
        <span
          role="tooltip"
          className="absolute left-0 top-full z-50 mt-1 max-w-xs whitespace-normal rounded-md border border-pc-border-strong bg-pc-elevated px-2.5 py-1.5 text-xs font-normal leading-snug text-pc-text shadow-lg"
        >
          {text}
        </span>
      ) : null}
    </span>
  );
}
