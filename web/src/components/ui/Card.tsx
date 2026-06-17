import type { ElementType, ReactNode, ComponentPropsWithoutRef } from 'react';

export interface CardProps {
  /** Element/tag to render as. Defaults to `div`. */
  as?: ElementType;
  /** Apply default interior padding (p-4). Defaults to `true`. */
  padded?: boolean;
  className?: string;
  children?: ReactNode;
}

/**
 * Surface container for the Operator Console.
 *
 * Neutral slate surface with a subtle border and the standard large radius.
 * Calm by default — no hover glow. Compose page content inside it.
 */
export function Card({
  as,
  padded = true,
  className = '',
  children,
  ...rest
}: CardProps & Omit<ComponentPropsWithoutRef<'div'>, keyof CardProps>) {
  const Tag = (as ?? 'div') as ElementType;
  const classes = [
    'bg-pc-surface',
    'border border-pc-border',
    'rounded-[var(--radius-lg)]',
    padded ? 'p-4' : '',
    className,
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <Tag className={classes} {...rest}>
      {children}
    </Tag>
  );
}

export default Card;
