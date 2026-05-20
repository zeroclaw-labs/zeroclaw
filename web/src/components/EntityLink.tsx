import { Link } from 'react-router-dom';
import type { ReactNode, CSSProperties, MouseEventHandler } from 'react';
import { entityConfigPath, type EntityKind } from '@/lib/entityLinks';

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
  return (
    <Link
      to={entityConfigPath(kind, id)}
      className={className}
      style={style}
      title={title ?? `Open ${kind} config: ${id}`}
      onClick={onClick}
    >
      {children ?? id}
    </Link>
  );
}
