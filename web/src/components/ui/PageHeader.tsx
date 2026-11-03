import type { ReactNode } from 'react';

export interface PageHeaderProps {
  title: ReactNode;
  description?: ReactNode;
  /** Right-aligned actions slot (buttons, etc.). */
  actions?: ReactNode;
  className?: string;
}

/**
 * Consistent page title row for the Operator Console: a strong title, an
 * optional muted description, and a right-aligned actions slot.
 */
export function PageHeader({ title, description, actions, className = '' }: PageHeaderProps) {
  const classes = [
    'flex items-start justify-between gap-4 flex-wrap',
    className,
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <div className={classes}>
      <div className="min-w-0 max-w-full">
        <h1 className="text-xl font-semibold leading-tight text-pc-text">{title}</h1>
        {description != null && (
          <p className="mt-1 text-sm text-pc-text-secondary break-words">{description}</p>
        )}
      </div>
      {actions != null && (
        <div className="flex items-center gap-2 flex-wrap justify-end w-full sm:w-auto min-w-0">{actions}</div>
      )}
    </div>
  );
}

export default PageHeader;
