import type { ButtonHTMLAttributes, ReactNode } from 'react';

export type ButtonVariant = 'primary' | 'ghost' | 'danger';
export type ButtonSize = 'sm' | 'md';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  children?: ReactNode;
}

const base =
  'inline-flex items-center justify-center gap-1.5 font-medium whitespace-nowrap ' +
  'rounded-[var(--radius-md)] border transition-colors duration-150 ' +
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] ' +
  'focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base ' +
  'disabled:opacity-40 disabled:cursor-not-allowed disabled:pointer-events-none ' +
  'cursor-pointer select-none';

const sizes: Record<ButtonSize, string> = {
  sm: 'h-7 px-2.5 text-[13px]',
  md: 'h-9 px-3.5 text-sm',
};

const variants: Record<ButtonVariant, string> = {
  // Accent fill with a fixed dark foreground that stays legible on the
  // light-to-mid accents this design system ships (>= AA on the Operator accents).
  primary:
    'bg-pc-accent border-transparent text-[#0b1220] hover:bg-pc-accent-light ' +
    'active:brightness-95',
  // Transparent until hovered — the calm default for secondary actions.
  // `--pc-hover` has no @theme utility, so the hover bg uses an arbitrary value.
  ghost:
    'bg-transparent border-pc-border text-pc-text-secondary ' +
    'hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong',
  // Tinted destructive action.
  danger:
    'bg-status-error/10 border-status-error/25 text-status-error ' +
    'hover:bg-status-error/15',
};

/**
 * Operator Console button.
 *
 * Three calm variants (primary accent, ghost, danger) with a strong
 * focus-visible ring. Forwards all native button props.
 */
export function Button({
  variant = 'primary',
  size = 'md',
  className = '',
  type,
  children,
  ...rest
}: ButtonProps) {
  const classes = [base, sizes[size], variants[variant], className]
    .filter(Boolean)
    .join(' ');

  return (
    <button type={type ?? 'button'} className={classes} {...rest}>
      {children}
    </button>
  );
}

export default Button;
