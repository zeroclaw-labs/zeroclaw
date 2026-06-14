import type { ReactNode } from 'react';
import { Card } from './Card';

export type StatTone = 'neutral' | 'ok' | 'warn' | 'error';

export interface StatCardProps {
  /** Small uppercase caption above the value. */
  label: string;
  /** The headline metric. */
  value: ReactNode;
  /** Optional supporting text below the value. */
  sublabel?: ReactNode;
  /** Optional leading icon (e.g. a lucide-react element). */
  icon?: ReactNode;
  /** Colors the value + icon. `neutral` uses the accent; others use status colors. */
  tone?: StatTone;
  className?: string;
}

const valueTone: Record<StatTone, string> = {
  neutral: 'text-pc-text',
  ok: 'text-status-success',
  warn: 'text-status-warning',
  error: 'text-status-error',
};

const iconTone: Record<StatTone, string> = {
  neutral: 'text-pc-accent',
  ok: 'text-status-success',
  warn: 'text-status-warning',
  error: 'text-status-error',
};

/**
 * Metric tile for the Operator Console dashboards. Big value, small uppercase
 * label, optional sublabel + icon. Tone tints the value and icon.
 */
export function StatCard({
  label,
  value,
  sublabel,
  icon,
  tone = 'neutral',
  className = '',
}: StatCardProps) {
  return (
    <Card className={className}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-[11px] font-medium uppercase tracking-wide text-pc-text-faint">
            {label}
          </div>
          <div className={`mt-1.5 text-2xl font-semibold leading-tight ${valueTone[tone]}`}>
            {value}
          </div>
          {sublabel != null && (
            <div className="mt-1 text-xs text-pc-text-muted">{sublabel}</div>
          )}
        </div>
        {icon != null && (
          <div className={`flex-shrink-0 ${iconTone[tone]}`} aria-hidden="true">
            {icon}
          </div>
        )}
      </div>
    </Card>
  );
}

export default StatCard;
