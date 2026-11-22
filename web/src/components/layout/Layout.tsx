import { useLayoutPreference } from '@/contexts/LayoutPreference';
import Sidebar from './Sidebar';
import { useEffect, useState } from 'react';
import { Navigate, useLocation } from 'react-router-dom';
import WorkspaceSettings from '@/components/WorkspaceSettings';
import Header from '@/components/layout/Header';
import ReloadBanner from '@/components/layout/ReloadBanner';
import UnsavedChangesBanner from '@/components/layout/UnsavedChangesBanner';
import { MainColumn } from '@/components/layout/MainColumn';
import CommandPalette, { useCommandPalette } from '@/components/CommandPalette';
import WorkspaceOutlet from './WorkspaceOutlet';
import WorkspaceKeyboard from './WorkspaceKeyboard';
import { NotAvailable } from '@/components/auth/NotAvailable';
import { useAuth } from '@/hooks/useAuth';
import { t } from '@/lib/i18n';
import { homePathFor, navAccessFor, pathAccess } from '@/lib/navAccess';

import { routeTitleKey } from '@/lib/navigation';

export default function Layout() {
  const { layout } = useLayoutPreference();
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const { pathname } = useLocation();
  const { open: paletteOpen, openPalette, closePalette } = useCommandPalette();
  // A signed-in account without the operator dashboard lands on chat or
  // config, and a deep link to another page says it isn't available. An
  // account with neither sees that at `/` too.
  const { session, viaRelay } = useAuth();
  const access = navAccessFor(session);
  const routeAccess = pathAccess(pathname, access);
  const home = homePathFor(access);

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
      name = t(layout === 'classic' ? 'nav.dashboard' : 'home.title');
    } else if (first === 'agent' && seg[1]) {
      name = `${decodeURIComponent(seg[1])} · ${t('nav.group.chat')}`;
    } else {
      const key = routeTitleKey(pathname);
      name = key ? t(key) : null;
    }
    document.title = name ? `${name} — ZeroClaw` : 'ZeroClaw';
  }, [pathname, layout]);

  return (
    <WorkspaceKeyboard access={access}><WorkspaceSettings><div data-workspace-window tabIndex={-1}
      role="region" aria-label={t('workspace.keyboard_workspace')} className="min-h-screen bg-pc-base text-pc-text">
      {layout === 'classic' && <Sidebar open={sidebarOpen} onClose={() => setSidebarOpen(false)} />}
      <MainColumn viaRelay={viaRelay} showRail={layout === 'classic'}>
        <Header onOpenPalette={openPalette} onOpenSidebar={() => setSidebarOpen(true)} />
        <ReloadBanner />
        <UnsavedChangesBanner />
        <div id="workspace-attention" />

        {/* Page content — ErrorBoundary keyed by the first path segment
            so the boundary resets when the user navigates between pages
            (e.g. /agent → /config), but stays mounted across param-only
            changes within a page (e.g. /config/providers → /config/browser).
            Keying on the full pathname remounted the entire route tree
            on every section click and reset scroll/state. */}
        <main className="flex-1 overflow-y-auto min-h-0">
            {pathname === '/' && !access.operatorPages && home ? (
              <Navigate to={home} replace />
            ) : routeAccess === 'allowed' ? (
              <WorkspaceOutlet />
            ) : (
              <NotAvailable access={routeAccess} />
            )}
        </main>
      </MainColumn>

      {/* Command palette — mounted once for the whole app. Toggled globally
          via ⌘K / Ctrl+K and from the Header search trigger. */}
      <CommandPalette open={paletteOpen} onClose={closePalette} />
    </div></WorkspaceSettings></WorkspaceKeyboard>
  );
}
