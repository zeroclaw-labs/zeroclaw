import { useState } from 'react';
import { useLocation } from 'react-router-dom';
import { LogOut, Menu, Settings } from 'lucide-react';
import { t } from '@/lib/i18n';
import { useLocaleContext } from '@/App';
import { useAuth } from '@/hooks/useAuth';
import { SettingsModal } from '@/components/SettingsModal';

const routeTitles: Record<string, string> = {
  '/': 'nav.dashboard',
  '/agent': 'nav.agent',
  '/tools': 'nav.tools',
  '/cron': 'nav.cron',
  '/integrations': 'nav.integrations',
  '/memory': 'nav.memory',
  '/config': 'nav.config',
  '/cost': 'nav.cost',
  '/logs': 'nav.logs',
  '/doctor': 'nav.doctor',
};

interface HeaderProps {
  onMenuToggle: () => void;
}

export default function Header({ onMenuToggle }: HeaderProps) {
  const location = useLocation();
  const { logout } = useAuth();
  const { locale, setAppLocale } = useLocaleContext();
  const [settingsOpen, setSettingsOpen] = useState(false);

  const titleKey = routeTitles[location.pathname] ?? 'nav.dashboard';
  const pageTitle = t(titleKey);

  const toggleLanguage = () => {
    // Cycle through: en -> zh -> tr -> en
    const nextLocale = locale === 'en' ? 'zh' : locale === 'zh' ? 'tr' : 'en';
    setAppLocale(nextLocale);
  };

  return (
    <>
      <header className="h-14 flex items-center justify-between px-6 border-b animate-fade-in" style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)', backdropFilter: 'blur(12px)', }}>
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

          {/* Page title */}
          <h1 className="h-9 leading-9 text-lg font-semibold tracking-tight" style={{ color: 'var(--pc-text-primary)' }}>{pageTitle}</h1>
        </div>

        {/* Right-side controls */}
        <div className="flex items-center gap-2 h-9">
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

          {/* Language switcher */}
          <button
            type="button"
            onClick={toggleLanguage}
            className="h-9 px-3 rounded-xl text-xs font-semibold border transition-all flex items-center"
            style={{
              borderColor: 'var(--pc-border)',
              color: 'var(--pc-text-secondary)',
              background: 'var(--pc-bg-elevated)',
            }}
            onMouseEnter={(e) => {
              e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
              e.currentTarget.style.color = 'var(--pc-text-primary)';
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.borderColor = 'var(--pc-border)';
              e.currentTarget.style.color = 'var(--pc-text-secondary)';
            }}
          >
            {locale === 'en' ? 'EN' : locale === 'zh' ? 'ZH' : 'TR'}
          </button>

          {/* Logout */}
          <button
            type="button"
            onClick={logout}
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
