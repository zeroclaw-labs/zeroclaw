import { useState, useEffect } from 'react';
import { Navigate, Outlet, useLocation } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';
import ReloadBanner from '@/components/layout/ReloadBanner';
import UnsavedChangesBanner from '@/components/layout/UnsavedChangesBanner';
import CommandPalette, { useCommandPalette } from '@/components/CommandPalette';
import { NotAvailable } from '@/components/auth/NotAvailable';
import { ErrorBoundary } from '@/App';
import { useAuth } from '@/hooks/useAuth';
import { t } from '@/lib/i18n';
import { homePathFor, navAccessFor, pathAccess } from '@/lib/navAccess';

// First-path-segment → i18n title key, so the browser tab/history/bookmark
// reflects the current page instead of a constant "ZeroClaw".
const TITLE_KEYS: Record<string, string> = {
  agents: 'nav.agents',
  config: 'nav.config',
  setup: 'nav.config',
  tools: 'nav.tools',
  skills: 'nav.skills',
  integrations: 'nav.integrations',
  cron: 'nav.cron',
  logs: 'nav.logs',
  doctor: 'nav.doctor',
  pairing: 'nav.pairing',
  canvas: 'nav.canvas',
  'acp-console': 'nav.acp',
  quickstart: 'nav.quickstart',
  memory: 'nav.memory',
};

export default function Layout() {
  const { pathname } = useLocation();
  const { open: paletteOpen, openPalette, closePalette } = useCommandPalette();
  const [sidebarOpen, setSidebarOpen] = useState(false);
  // A signed-in account without the operator dashboard lands on chat or
  // config, and a deep link to another page says it isn't available.
  const { session } = useAuth();
  const access = navAccessFor(session);
  const routeAccess = pathAccess(pathname, access);

  // Close the mobile drawer on route change.
  useEffect(() => {
    setSidebarOpen(false);
  }, [pathname]);

  // Per-route browser tab title.
  useEffect(() => {
    const seg = pathname.split('/').filter(Boolean);
    const first = seg[0];
    let name: string | null;
    if (!first) {
      name = t('nav.dashboard');
    } else if (first === 'agent' && seg[1]) {
      name = `${decodeURIComponent(seg[1])} · ${t('nav.group.chat')}`;
    } else {
      const key = TITLE_KEYS[first];
      name = key ? t(key) : null;
    }
    document.title = name ? `${name} — ZeroClaw` : 'ZeroClaw';
  }, [pathname]);

  return (
    <div className="min-h-screen bg-pc-base text-pc-text">
      {/* Fixed slim icon rail (desktop) + drawer (mobile). */}
      <Sidebar open={sidebarOpen} onClose={() => setSidebarOpen(false)} />

      {/* Main area — offset by the fixed 56px rail on desktop, full-width on
          mobile. The rail is always slim, so the offset is constant. */}
      <div className="flex flex-col flex-1 min-w-0 h-screen md:ml-14 ml-0">
        <Header
          onMenuToggle={() => setSidebarOpen((v) => !v)}
          onOpenPalette={openPalette}
        />
        <ReloadBanner />
        <UnsavedChangesBanner />

        {/* Page content — ErrorBoundary keyed by the first path segment
            so the boundary resets when the user navigates between pages
            (e.g. /agent → /config), but stays mounted across param-only
            changes within a page (e.g. /config/providers → /config/browser).
            Keying on the full pathname remounted the entire route tree
            on every section click and reset scroll/state. */}
        <main className="flex-1 overflow-y-auto min-h-0">
          <ErrorBoundary key={pathname.split('/')[1] ?? ''}>
            {pathname === '/' && !access.operatorPages ? (
              <Navigate to={homePathFor(access)} replace />
            ) : routeAccess === 'allowed' ? (
              <Outlet />
            ) : (
              <NotAvailable access={routeAccess} />
            )}
          </ErrorBoundary>
        </main>
      </div>

      {/* Command palette — mounted once for the whole app. Toggled globally
          via ⌘K / Ctrl+K and from the Header search trigger. */}
      <CommandPalette open={paletteOpen} onClose={closePalette} />
    </div>
  );
}
