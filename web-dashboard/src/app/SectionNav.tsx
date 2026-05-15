/**
 * Shared section navigation rail (M5.0).
 *
 * Before M5.0, both ChatPage (sidebar nav row) and BoardPage (header
 * row) hardcoded the Chat/Board link pair. Adding Overview as a third
 * destination meant editing both inline blocks AND keeping their
 * active-tab styling consistent. This component centralises that —
 * each page now renders <SectionNav /> and the nav reads
 * `useLocation().pathname` to mark the active tab.
 *
 * Styling rules (must match the inline nav this replaces):
 *   - Active tab:   `font-medium underline`, `--color-text`
 *   - Inactive tab: `hover:underline`, `--color-text-muted`
 *
 * The component is presentation-only: it has no internal state, no
 * data dependencies, and no behavior beyond routing. That keeps it
 * trivially reusable in every page header.
 */
import { Link, useLocation } from "react-router-dom";

interface NavItem {
  to: string;
  label: string;
  /** Match this prefix for active styling. `/chat/123` should still mark `/chat` active. */
  prefix: string;
}

const ITEMS: NavItem[] = [
  { to: "/chat", label: "Chat", prefix: "/chat" },
  { to: "/board", label: "Board", prefix: "/board" },
  { to: "/overview", label: "Overview", prefix: "/overview" },
];

interface SectionNavProps {
  /** Layout mode. `inline` matches the BoardPage header row. `stacked`
   * matches the ChatPage left-rail row (slightly tighter padding). */
  layout?: "inline" | "stacked";
}

export function SectionNav({ layout = "inline" }: SectionNavProps) {
  const { pathname } = useLocation();

  const containerClass =
    layout === "inline"
      ? "flex items-center gap-3 text-sm"
      : "flex items-center gap-1 px-3 py-1 border-b text-xs";

  const containerStyle =
    layout === "stacked"
      ? { borderColor: "var(--color-border)" }
      : undefined;

  return (
    <nav
      className={containerClass}
      style={containerStyle}
      aria-label="Dashboard sections"
    >
      {ITEMS.map((item, idx) => {
        const isActive = pathname === item.prefix
          || pathname.startsWith(`${item.prefix}/`);
        return (
          <span key={item.to} className="flex items-center gap-3">
            {idx > 0 && layout === "inline" ? (
              <span style={{ color: "var(--color-text-muted)" }}>·</span>
            ) : null}
            <Link
              to={item.to}
              aria-current={isActive ? "page" : undefined}
              className={
                isActive
                  ? "font-medium underline px-1"
                  : "hover:underline px-1"
              }
              style={{
                color: isActive
                  ? "var(--color-text)"
                  : "var(--color-text-muted)",
              }}
            >
              {item.label}
            </Link>
          </span>
        );
      })}
    </nav>
  );
}
