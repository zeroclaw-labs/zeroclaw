import { useState, forwardRef, type ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';
import Toggle from './Toggle';

interface SectionCardProps {
  icon: ReactNode;
  title: string;
  enabled?: boolean;
  onToggleEnabled?: (v: boolean) => void;
  defaultOpen?: boolean;
  children: ReactNode;
}

const SectionCard = forwardRef<HTMLDivElement, SectionCardProps>(
  ({ icon, title, enabled, onToggleEnabled, defaultOpen = false, children }, ref) => {
    const [open, setOpen] = useState(defaultOpen);

    return (
      <div ref={ref} className="card rounded-2xl overflow-hidden">
        <button
          type="button"
          onClick={() => setOpen(!open)}
          className="w-full flex items-center gap-3 px-5 py-4 text-left"
          style={{ background: 'var(--pc-bg-surface)' }}
        >
          <span style={{ color: 'var(--pc-accent)' }}>{icon}</span>
          <span className="text-sm font-semibold flex-1" style={{ color: 'var(--pc-text-primary)' }}>
            {title}
          </span>
          {onToggleEnabled !== undefined && enabled !== undefined && (
            <span onClick={(e) => e.stopPropagation()}>
              <Toggle value={enabled} onChange={onToggleEnabled} />
            </span>
          )}
          <ChevronDown
            className="h-4 w-4 transition-transform duration-200"
            style={{
              color: 'var(--pc-text-muted)',
              transform: open ? 'rotate(180deg)' : 'rotate(0deg)',
            }}
          />
        </button>
        {open && (
          <div className="px-5 pb-4 divide-y" style={{ borderColor: 'var(--pc-border)' }}>
            {children}
          </div>
        )}
      </div>
    );
  },
);

SectionCard.displayName = 'SectionCard';
export default SectionCard;
