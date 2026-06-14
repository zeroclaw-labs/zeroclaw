import { useState, useEffect } from 'react';
import { Outlet, useLocation } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';
import ReloadBanner from '@/components/layout/ReloadBanner';
import UnsavedChangesBanner from '@/components/layout/UnsavedChangesBanner';
import CommandPalette, { useCommandPalette } from '@/components/CommandPalette';
import { ErrorBoundary } from '@/App';

export default function Layout() {
  const { pathname } = useLocation();
  const { open: paletteOpen, openPalette, closePalette } = useCommandPalette();
  const [sidebarOpen, setSidebarOpen] = useState(false);

  // Close the mobile drawer on route change.
  useEffect(() => {
    setSidebarOpen(false);
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
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>

      {/* Command palette — mounted once for the whole app. Toggled globally
          via ⌘K / Ctrl+K and from the Header search trigger. */}
      <CommandPalette open={paletteOpen} onClose={closePalette} />
    </div>
  );
}
