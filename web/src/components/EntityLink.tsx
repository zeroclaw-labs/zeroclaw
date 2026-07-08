import { Link } from 'react-router-dom';
import type { ReactNode, CSSProperties, MouseEventHandler } from 'react';
import { entityConfigPath, type EntityKind } from '@/lib/entityLinks';
import { t } from '@/lib/i18n';

export interface EntityLinkProps {
  kind: EntityKind;
  id: string;
  className?: string;
  style?: CSSProperties;
  title?: string;
  children?: ReactNode;
  /** Stop propagation so the link works inside a clickable parent row. */
  stopPropagation?: boolean;
}

export default function EntityLink({
  kind,
  id,
  className,
  style,
  title,
  children,
  stopPropagation = true,
}: EntityLinkProps) {
  const onClick: MouseEventHandler = stopPropagation
    ? (e) => e.stopPropagation()
    : () => {};
  // Tokenized link color via --pc-text-link (tracks the per-theme accent),
  // a calm hover underline, and a strong focus-visible ring. Callers can
  // still override color through `className`/`style`.
  return (
    <Link
      to={entityConfigPath(kind, id)}
      className={[
        'underline-offset-2 hover:underline rounded-[var(--radius-sm)]',
        'transition-colors duration-150',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]',
        'focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base',
        className ?? '',
      ]
        .filter(Boolean)
        .join(' ')}
      style={{ color: 'var(--pc-text-link)', ...style }}
      title={title ?? `${t('entity_link.open_prefix')}${kind}${t('entity_link.config_sep')}${id}`}
      onClick={onClick}
    >
      {children ?? id}
    </Link>
  );
}
