// Generic horizontal tab strip for config sections that have more than
// one authoring surface (Agents: Settings + Personality; Model
// providers: Connection / Model / Advanced; ...). Tab state lives in the
// `?tab=` URL query so navigation + reloads keep the active tab. The
// component is presentation-only — tab content is supplied by callers.

import { useMemo } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';
import type { ReactNode } from 'react';

export interface SectionTabSpec {
  /** URL-safe key written into `?tab=...`. */
  key: string;
  /** Display label for the tab button. */
  label: string;
  /** Rendered when this tab is active. */
  render: () => ReactNode;
}

interface SectionTabsProps {
  tabs: SectionTabSpec[];
  /** Tab key to activate when the URL has no `tab` query. Defaults to
   *  the first tab. */
  defaultKey?: string;
}

export default function SectionTabs({ tabs, defaultKey }: SectionTabsProps) {
  const location = useLocation();
  const navigate = useNavigate();

  const activeKey = useMemo(() => {
    const params = new URLSearchParams(location.search);
    const fromUrl = params.get('tab');
    if (fromUrl && tabs.some((t) => t.key === fromUrl)) return fromUrl;
    return defaultKey ?? tabs[0]?.key ?? '';
  }, [location.search, tabs, defaultKey]);

  const setActive = (key: string) => {
    const params = new URLSearchParams(location.search);
    params.set('tab', key);
    navigate(
      { pathname: location.pathname, search: `?${params.toString()}` },
      { replace: true },
    );
  };

  const active = tabs.find((t) => t.key === activeKey) ?? tabs[0];
  if (!active) return null;

  return (
    <div className="flex flex-col gap-4 flex-1 min-h-0">
      {/* Calm underline tabs: active draws an accent underline + primary
          text; inactive sits muted with a transparent border. The shared
          bottom hairline reads as a quiet baseline, not a heavy bar. */}
      <div
        className="flex items-center gap-1 border-b border-pc-border -mx-2 px-2 overflow-x-auto"
        role="tablist"
      >
        {tabs.map((t) => {
          const isActive = t.key === active.key;
          return (
            <button
              key={t.key}
              type="button"
              role="tab"
              aria-selected={isActive}
              onClick={() => setActive(t.key)}
              className={[
                'px-3 py-2 text-sm border-b-2 -mb-px transition-colors',
                'focus-visible:outline-none focus-visible:ring-2',
                'focus-visible:ring-[var(--pc-focus)] focus-visible:rounded-sm',
                isActive
                  ? 'border-pc-accent text-pc-text font-medium'
                  : 'border-transparent text-pc-text-muted hover:text-pc-text-secondary',
              ].join(' ')}
            >
              {t.label}
            </button>
          );
        })}
      </div>
      <div role="tabpanel" className="flex-1 min-h-0 flex flex-col">
        {active.render()}
      </div>
    </div>
  );
}
