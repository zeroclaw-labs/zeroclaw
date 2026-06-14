import { useState, useRef, useEffect } from 'react';
import { useLocation } from 'react-router-dom';
import { LogOut, Settings, ChevronDown, PanelLeftClose, PanelLeftOpen, Menu, Globe, Search } from 'lucide-react';
import { t, SUPPORTED_LOCALES } from '@/lib/i18n';
import { useLocaleContext } from '@/App';
import { useAuth } from '@/hooks/useAuth';
import { SettingsModal } from '@/components/SettingsModal';

// Exact-path titles. The dashboard ('/') must stay exact so it doesn't
// swallow every other route as a prefix.
const exactRouteTitles: Record<string, string> = {
  '/': 'nav.dashboard',
};

// Section titles keyed by the first path segment. Resolving by the matched
// section (not the literal pathname) means nested routes like /config/agents,
// /agent/:alias, or /agents all surface a correct <h1> instead of an empty one.
const sectionTitles: Record<string, string> = {
  agent: 'nav.agent',
  agents: 'nav.agents',
  tools: 'nav.tools',
  cron: 'nav.cron',
  integrations: 'nav.integrations',
  config: 'nav.config',
  setup: 'nav.config',
  memory: 'nav.memory',
  logs: 'nav.logs',
  doctor: 'nav.doctor',
  pairing: 'nav.doctor',
  canvas: 'nav.canvas',
  'acp-console': 'nav.acp',
  quickstart: 'nav.quickstart',
};

// Derive the i18n title key from the matched route/section so every page —
// including nested config routes — renders a non-empty heading. Unknown routes
// fall back to an empty title rather than mislabeling them.
function titleKeyFor(pathname: string): string | undefined {
  if (exactRouteTitles[pathname]) return exactRouteTitles[pathname];
  const section = pathname.split('/').filter(Boolean)[0];
  return section ? sectionTitles[section] : undefined;
}

interface HeaderProps {
  onMenuToggle: () => void;
  onCollapseToggle: () => void;
  collapsed: boolean;
  onOpenPalette: () => void;
}

export default function Header({ onMenuToggle, onCollapseToggle, collapsed, onOpenPalette }: HeaderProps) {
  const location = useLocation();
  const { logout } = useAuth();
  const { locale, setAppLocale } = useLocaleContext();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [langOpen, setLangOpen] = useState(false);
  const langRef = useRef<HTMLDivElement>(null);

  // Fall back to a plain title for unknown routes rather than mislabeling
  // them as "Dashboard" — e.g. early /quickstart hits before the entry was
  // mapped here showed "Dashboard" for the first-run flow.
  const titleKey = titleKeyFor(location.pathname);
  const pageTitle = titleKey ? t(titleKey) : '';

  const handleLogout = () => {
    if (window.confirm(t('auth.logout_confirm'))) {
      logout();
    }
  };

  // Close dropdown when clicking outside
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (langRef.current && !langRef.current.contains(e.target as Node)) {
        setLangOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, []);

  return (
    <>
      <header className="h-14 flex items-center justify-between px-6 border-b animate-fade-in relative" style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)', backdropFilter: 'blur(12px)', zIndex: 100 }}>
        <div className="flex items-center gap-3">
          {/* Hamburger — visible only on mobile */}
          <button
            type="button"
            onClick={onMenuToggle}
            className="md:hidden p-1.5 -ml-1.5 rounded-lg transition-colors duration-200"
            style={{ color: 'var(--pc-text-muted)' }}
            onMouseEnter={(e) => { e.currentTarget.style.color = 'var(--pc-text-primary)'; e.currentTarget.style.background = 'var(--pc-hover)'; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = 'var(--pc-text-muted)'; e.currentTarget.style.background = 'transparent'; }}
            aria-label="Open menu"
          >
            <Menu className="h-5 w-5" />
          </button>

          {/* Collapse toggle — visible only on desktop */}
          <button
            type="button"
            onClick={onCollapseToggle}
            className="hidden md:flex p-1.5 -ml-1.5 rounded-lg transition-colors duration-200"
            style={{ color: 'var(--pc-text-muted)' }}
            onMouseEnter={(e) => { e.currentTarget.style.color = 'var(--pc-text-primary)'; e.currentTarget.style.background = 'var(--pc-hover)'; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = 'var(--pc-text-muted)'; e.currentTarget.style.background = 'transparent'; }}
            aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          >
            {collapsed ? <PanelLeftOpen className="h-5 w-5" /> : <PanelLeftClose className="h-5 w-5" />}
          </button>

          {/* Page title */}
          <h1 className="h-9 leading-9 text-lg font-semibold tracking-tight" style={{ color: 'var(--pc-text-primary)' }}>{pageTitle}</h1>
        </div>

        {/* Right-side controls */}
        <div className="flex items-center gap-2 h-9">
          {/* Command-palette trigger — styled like a search field. Opens the
              palette; the same action is bound globally to ⌘K / Ctrl+K, shown
              as a hint chip on the right. */}
          <button
            type="button"
            onClick={onOpenPalette}
            className="hidden sm:flex h-9 items-center gap-2 rounded-[var(--radius-md)] border border-pc-border bg-pc-input pl-2.5 pr-2 text-sm text-pc-text-muted transition-colors hover:border-pc-border-strong hover:text-pc-text-secondary hover:bg-[var(--pc-hover)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-surface"
            aria-label={t('nav.cmdk.placeholder')}
          >
            <Search className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
            <span className="hidden md:inline w-32 text-left">{t('nav.cmdk.placeholder')}</span>
            <kbd className="ml-1 flex items-center gap-0.5 rounded-[var(--radius-sm)] border border-pc-border bg-pc-elevated px-1.5 py-0.5 text-[11px] font-mono text-pc-text-faint">
              <span className="text-[13px] leading-none">⌘</span>K
            </kbd>
          </button>

          {/* Command-palette trigger — compact icon-only on the smallest screens. */}
          <button
            type="button"
            onClick={onOpenPalette}
            className="sm:hidden h-9 w-9 flex items-center justify-center rounded-[var(--radius-md)] text-pc-text-muted transition-colors hover:text-pc-text hover:bg-[var(--pc-hover)]"
            aria-label={t('nav.cmdk.placeholder')}
          >
            <Search className="h-3.5 w-3.5" />
          </button>

          {/* Settings */}
          <button
            type="button"
            onClick={() => setSettingsOpen(true)}
            className="h-9 w-9 flex items-center justify-center rounded-xl text-xs transition-all"
            style={{ color: 'var(--pc-text-muted)', background: 'transparent', border: 'none', cursor: 'pointer' }}
            onMouseEnter={(e) => { e.currentTarget.style.color = 'var(--pc-text-primary)'; e.currentTarget.style.background = 'var(--pc-hover)'; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = 'var(--pc-text-muted)'; e.currentTarget.style.background = 'transparent'; }}
            aria-label={t('settings.title')}
          >
            <Settings className="h-3.5 w-3.5" />
          </button>

          {/* Language switcher dropdown */}
          <div ref={langRef} className="relative" style={{ zIndex: 9999 }}>
            <button
              type="button"
              onClick={() => setLangOpen(!langOpen)}
              className="h-9 px-3 rounded-xl text-xs font-semibold border transition-all flex items-center gap-1.5"
              style={{
                borderColor: langOpen ? 'var(--pc-accent-dim)' : 'var(--pc-border)',
                color: langOpen ? 'var(--pc-text-primary)' : 'var(--pc-text-secondary)',
                background: 'var(--pc-bg-elevated)',
              }}
              onMouseEnter={(e) => {
                e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
                e.currentTarget.style.color = 'var(--pc-text-primary)';
              }}
              onMouseLeave={(e) => {
                if (!langOpen) {
                  e.currentTarget.style.borderColor = 'var(--pc-border)';
                  e.currentTarget.style.color = 'var(--pc-text-secondary)';
                }
              }}
            >
              <Globe className="h-3.5 w-3.5" />
              {locale.toUpperCase()}
              <ChevronDown className="h-3 w-3" style={{ transform: langOpen ? 'rotate(180deg)' : undefined, transition: 'transform 0.15s' }} />
            </button>

            {langOpen && (
              <div
                className="absolute right-0 top-full mt-1 rounded-xl border overflow-hidden shadow-lg"
                style={{
                  background: 'var(--pc-bg-elevated)',
                  borderColor: 'var(--pc-border)',
                  maxHeight: '360px',
                  overflowY: 'auto',
                  minWidth: '200px',
                  zIndex: 9999,
                }}
              >
                {SUPPORTED_LOCALES.map(({ code, name }) => (
                  <button
                    key={code}
                    type="button"
                    onClick={() => {
                      setAppLocale(code);
                      setLangOpen(false);
                    }}
                    className="w-full px-3 py-2 text-xs text-left flex items-center gap-2.5 transition-colors"
                    style={{
                      color: code === locale ? 'var(--pc-accent)' : 'var(--pc-text-secondary)',
                      background: code === locale ? 'var(--pc-accent-glow)' : 'transparent',
                      fontWeight: code === locale ? 600 : 400,
                    }}
                    onMouseEnter={(e) => {
                      if (code !== locale) {
                        e.currentTarget.style.background = 'var(--pc-hover)';
                        e.currentTarget.style.color = 'var(--pc-text-primary)';
                      }
                    }}
                    onMouseLeave={(e) => {
                      if (code !== locale) {
                        e.currentTarget.style.background = 'transparent';
                        e.currentTarget.style.color = 'var(--pc-text-secondary)';
                      }
                    }}
                  >
                    <span className="flex-1">{name}</span>
                    <span className="font-mono opacity-40">{code.toUpperCase()}</span>
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Logout */}
          <button
            type="button"
            onClick={handleLogout}
            className="h-9 px-3 rounded-xl text-xs transition-all flex items-center gap-1.5"
            style={{ color: 'var(--pc-text-muted)', background: 'transparent', border: 'none', cursor: 'pointer' }}
            onMouseEnter={(e) => {
              e.currentTarget.style.color = 'var(--color-status-error)';
              e.currentTarget.style.background = 'var(--color-status-error-alpha-08)';
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.color = 'var(--pc-text-muted)';
              e.currentTarget.style.background = 'transparent';
            }}
          >
            <LogOut className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">{t('auth.logout')}</span>
          </button>
        </div>
      </header>

      <SettingsModal open={settingsOpen} onClose={() => setSettingsOpen(false)} />
    </>
  );
}
