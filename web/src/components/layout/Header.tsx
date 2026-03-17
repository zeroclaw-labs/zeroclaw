import { useLocation } from 'react-router-dom';
import { LogOut, Menu } from 'lucide-react';
import { t } from '@/lib/i18n';
import { useLocaleContext } from '@/App';
import { useAuth } from '@/hooks/useAuth';

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

  const titleKey = routeTitles[location.pathname] ?? 'nav.dashboard';
  const pageTitle = t(titleKey);

  const toggleLanguage = () => {
    // Cycle through: en -> zh -> tr -> en
    const nextLocale = locale === 'en' ? 'zh' : locale === 'zh' ? 'tr' : 'en';
    setAppLocale(nextLocale);
  };

  return (
    <header className="h-14 flex items-center justify-between px-6 border-b border-[#1a1a3e]/40 animate-fade-in" style={{ background: 'linear-gradient(90deg, rgba(8,8,24,0.9), rgba(5,5,16,0.9))', backdropFilter: 'blur(12px)' }}>
      <div className="flex items-center gap-3">
        {/* Hamburger — visible only on mobile */}
        <button
          type="button"
          onClick={onMenuToggle}
          className="md:hidden p-1.5 -ml-1.5 rounded-lg text-[#8892a8] hover:text-white hover:bg-[#0080ff10] transition-colors duration-200"
          aria-label="Open menu"
        >
          <Menu className="h-5 w-5" />
        </button>

        {/* Page title */}
        <h1 className="text-lg font-semibold text-white tracking-tight">{pageTitle}</h1>
      </div>

      {/* Right-side controls */}
      <div className="flex items-center gap-3">
        {/* Language switcher */}
        <button
          type="button"
          onClick={toggleLanguage}
          className="px-3 py-1 rounded-lg text-xs font-semibold border border-[#1a1a3e] text-[#8892a8] hover:text-white hover:border-[#0080ff40] hover:bg-[#0080ff10] transition-all duration-300"
        >
          {locale === 'en' ? 'EN' : 'TR'}
        </button>

        {/* Logout */}
        <button
          type="button"
          onClick={logout}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs text-[#8892a8] hover:text-[#ff4466] hover:bg-[#ff446610] transition-all duration-300"
        >
          <LogOut className="h-3.5 w-3.5" />
          <span className="hidden sm:inline">{t('auth.logout')}</span>
        </button>
      </div>
    </header>
  );
}
