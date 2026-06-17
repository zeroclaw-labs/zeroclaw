import type { ReactNode } from 'react';

export type BadgeTone = 'neutral' | 'ok' | 'warn' | 'error';

export interface BadgeProps {
  tone?: BadgeTone;
  className?: string;
  children: ReactNode;
}

const tones: Record<BadgeTone, string> = {
  neutral: 'bg-pc-elevated text-pc-text-secondary border-pc-border',
  ok: 'bg-status-success/10 text-status-success border-status-success/20',
  warn: 'bg-status-warning/10 text-status-warning border-status-warning/20',
  error: 'bg-status-error/10 text-status-error border-status-error/20',
};

/**
 * Small status pill for the Operator Console. Subtle tinted background keyed to
 * the tone, with a matching border. Keep the content short (a word or two).
 */
export function Badge({ tone = 'neutral', className = '', children }: BadgeProps) {
  const classes = [
    'inline-flex items-center gap-1 align-middle',
    'px-2 py-0.5 rounded-full border',
    'text-[11px] font-medium leading-none tracking-wide',
    tones[tone],
    className,
  ]
    .filter(Boolean)
    .join(' ');

  return <span className={classes}>{children}</span>;
}

export default Badge;
