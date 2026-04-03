import type { ReactNode } from 'react';

interface FieldRowProps {
  label: string;
  description?: string;
  children: ReactNode;
}

export default function FieldRow({ label, description, children }: FieldRowProps) {
  return (
    <div className="flex items-center justify-between gap-4 py-3 px-1">
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>{label}</div>
        {description && (
          <div className="text-xs mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>{description}</div>
        )}
      </div>
      <div className="flex-shrink-0">{children}</div>
    </div>
  );
}
