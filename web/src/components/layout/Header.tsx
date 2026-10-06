import { useState } from 'react';
import { Link, useLocation } from 'react-router-dom';
import { Home, MoreHorizontal, Search } from 'lucide-react';
import { t, SUPPORTED_LOCALES } from '@/lib/i18n';
import { useLocaleContext } from '@/App';
import { useAuth } from '@/hooks/useAuth';
import { SettingsModal } from '@/components/SettingsModal';
import { useWorkspaceSettings } from '@/components/WorkspaceSettings';
import { activeWorkspace } from '@/lib/navigation';

export default function Header({
  onOpenPalette,
}: {
  onOpenPalette: () => void;
}) {
  const { pathname } = useLocation();
  const workspace = activeWorkspace(pathname);
  const { logout } = useAuth();
  const { locale, setAppLocale } = useLocaleContext();
  const openSettings = useWorkspaceSettings();
  const [appearance, setAppearance] = useState(false);
  const [menu, setMenu] = useState(false);
  return (
    <>
      <header className="relative z-40 grid min-h-16 shrink-0 grid-cols-[1fr_auto_1fr] items-center gap-y-2 border-b border-pc-border bg-pc-surface px-2 py-2 sm:px-6 lg:py-0">
        <Link
          to="/"
          aria-label={t('home.title')}
          className="flex w-fit items-center gap-2 rounded-lg p-1.5 text-sm font-semibold tracking-tight text-pc-text-secondary hover:text-pc-text"
        >
          <Home className="h-4 w-4 sm:hidden" />
          <span className="hidden sm:inline">ZeroClaw</span>
        </Link>
        <nav
          aria-label={t('workspace.choose')}
          className="flex items-center gap-1 rounded-xl border border-pc-border bg-pc-base p-1 text-sm"
        >
          {[
            ['/agent', 'workspace.agent'],
            ['/code', 'nav.code'],
            ['/sops', 'workspace.sop'],
            ['/admin', 'workspace.admin'],
          ].map(([to, label]) => {
            const active = workspace === to;
            return (
              <Link
                key={to}
                to={to!}
                aria-current={active ? 'page' : undefined}
                className={`rounded-lg px-1.5 py-1.5 text-xs font-medium min-[400px]:px-2 min-[400px]:text-sm transition-colors sm:px-5 ${active ? 'bg-pc-elevated text-pc-accent shadow-sm' : 'text-pc-text-secondary hover:bg-pc-surface hover:text-pc-text'}`}
              >
                {t(label!)}
              </Link>
            );
          })}
        </nav>
        <div className="contents lg:flex lg:items-center lg:justify-end lg:gap-2">
          <button
            type="button"
            onClick={onOpenPalette}
            aria-label={t('workspace.search_settings')}
            aria-keyshortcuts="Meta+K Control+K"
            className="col-span-3 row-start-2 flex min-w-0 items-center gap-2 rounded-lg border border-pc-border bg-pc-base px-3 py-2 text-pc-text-secondary hover:bg-pc-elevated hover:text-pc-text"
          >
            <Search className="h-4 w-4 shrink-0" aria-hidden="true" />
            <span className="min-w-0 flex-1 truncate text-left text-xs">{t('workspace.search_settings')}</span>
            <kbd className="shrink-0 whitespace-nowrap rounded border border-pc-border px-1.5 py-0.5 text-xs">⌘ K / Ctrl K</kbd>
          </button>
          <button
            type="button"
            onClick={() => setMenu(!menu)}
            aria-expanded={menu}
            aria-label={t('workspace.more')}
            className="col-start-3 row-start-1 justify-self-end rounded-lg p-1.5 text-pc-text-muted hover:bg-pc-elevated hover:text-pc-text sm:p-2"
          >
            <MoreHorizontal className="h-4 w-4" />
          </button>
        </div>
        {menu && (
          <>
            <button
              type="button"
              aria-label={t('common.close')}
              className="fixed inset-0 cursor-default"
              onClick={() => setMenu(false)}
            />
            <div className="absolute right-4 top-14 z-10 w-56 rounded-xl border border-pc-border bg-pc-surface p-2 text-sm shadow-xl">
              <button
                className="w-full rounded-lg p-2 text-left hover:bg-pc-elevated"
                onClick={() => {
                  setMenu(false);
                  openSettings('/config');
                }}
              >
                {t('config.all_settings')}
              </button>
              <button
                className="w-full rounded-lg p-2 text-left hover:bg-pc-elevated"
                onClick={() => {
                  setMenu(false);
                  setAppearance(true);
                }}
              >
                {t('nav.appearance')}
              </button>
              <label className="flex items-center gap-2 p-2 text-pc-text-muted">
                {t('settings.language')}
                <select
                  aria-label={t('settings.language')}
                  className="min-w-0 flex-1 bg-pc-surface text-pc-text"
                  value={locale}
                  onChange={(e) => setAppLocale(e.target.value)}
                >
                  {SUPPORTED_LOCALES.map(({ code, name }) => (
                    <option key={code} value={code}>
                      {name}
                    </option>
                  ))}
                </select>
              </label>
              <button
                className="w-full rounded-lg p-2 text-left hover:bg-pc-elevated"
                onClick={() => {
                  if (window.confirm(t('auth.logout_confirm'))) logout();
                }}
              >
                {t('auth.logout')}
              </button>
            </div>
          </>
        )}
      </header>
      <SettingsModal open={appearance} onClose={() => setAppearance(false)} />
    </>
  );
}
