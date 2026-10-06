import { useEffect } from 'react';
import { useLocation } from 'react-router-dom';
import WorkspaceSettings from '@/components/WorkspaceSettings';
import Header from '@/components/layout/Header';
import ReloadBanner from '@/components/layout/ReloadBanner';
import UnsavedChangesBanner from '@/components/layout/UnsavedChangesBanner';
import CommandPalette, { useCommandPalette } from '@/components/CommandPalette';
import WorkspaceOutlet from './WorkspaceOutlet';
import { t } from '@/lib/i18n';

import { routeTitleKey } from '@/lib/navigation';

export default function Layout() {
  const { pathname } = useLocation();
  const { open: paletteOpen, openPalette, closePalette } = useCommandPalette();
  // Per-route browser tab title.
  useEffect(() => {
    const seg = pathname.split('/').filter(Boolean);
    const first = seg[0];
    let name: string | null;
    if (!first) {
      name = t('home.title');
    } else if (first === 'agent' && seg[1]) {
      name = `${decodeURIComponent(seg[1])} · ${t('nav.group.chat')}`;
    } else {
      const key = routeTitleKey(pathname);
      name = key ? t(key) : null;
    }
    document.title = name ? `${name} — ZeroClaw` : 'ZeroClaw';
  }, [pathname]);

  return (
    <WorkspaceSettings><div className="min-h-screen bg-pc-base text-pc-text">
      <div className="flex h-dvh min-w-0 flex-col">
        <Header onOpenPalette={openPalette} />
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
          <WorkspaceOutlet />
        </main>
      </div>

      {/* Command palette — mounted once for the whole app. Toggled globally
          via ⌘K / Ctrl+K and from the Header search trigger. */}
      <CommandPalette open={paletteOpen} onClose={closePalette} />
    </div></WorkspaceSettings>
  );
}
