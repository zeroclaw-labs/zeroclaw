import { useLocation, useNavigate } from 'react-router-dom';
import { useState, useEffect } from 'react';
import { LogOut, Menu, Coins } from 'lucide-react';
import { t } from '@/lib/i18n';
import type { Locale } from '@/lib/i18n';
import { useLocaleContext } from '@/App';
import { useAuth } from '@/hooks/useAuth';
import { getCreditBalance } from '@/lib/api';

const routeTitles: Record<string, string> = {
  '/': 'nav.dashboard',
  '/agent': 'nav.agent',
  '/tools': 'nav.tools',
  '/cron': 'nav.cron',
  '/integrations': 'nav.integrations',
  '/memory': 'nav.memory',
  '/config': 'nav.config',
  '/credits': 'nav.credits',
  '/cost': 'nav.cost',
  '/logs': 'nav.logs',
  '/doctor': 'nav.doctor',
};

const localeCycle: Locale[] = ['en', 'tr', 'zh-CN'];

interface HeaderProps {
  onToggleSidebar: () => void;
}

export default function Header({ onToggleSidebar }: HeaderProps) {
  const location = useLocation();
  const navigate = useNavigate();
  const { logout } = useAuth();
  const { locale, setAppLocale } = useLocaleContext();
  const [creditBalance, setCreditBalance] = useState<number | null>(null);

  useEffect(() => {
    getCreditBalance()
      .then((data) => {
        if (data.enabled) setCreditBalance(data.balance);
      })
      .catch(() => { /* billing not available */ });

    // Refresh balance every 30 seconds
    const interval = setInterval(() => {
      getCreditBalance()
        .then((data) => {
          if (data.enabled) setCreditBalance(data.balance);
        })
        .catch(() => {});
    }, 30_000);
    return () => clearInterval(interval);
  }, []);

  const titleKey = routeTitles[location.pathname] ?? 'nav.dashboard';
  const pageTitle = t(titleKey);

  const toggleLanguage = () => {
    const currentIndex = localeCycle.indexOf(locale);
    const nextLocale = localeCycle[(currentIndex + 1) % localeCycle.length] ?? 'en';
    setAppLocale(nextLocale);
  };

  return (
    <header className="h-14 bg-gray-800 border-b border-gray-700 flex items-center justify-between px-4 md:px-6">
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onToggleSidebar}
          aria-label="Open navigation"
          className="md:hidden p-1.5 rounded-md text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
        >
          <Menu className="h-5 w-5" />
        </button>
        <h1 className="text-lg font-semibold text-white">{pageTitle}</h1>
      </div>

      <div className="flex items-center gap-2 md:gap-4">
        {creditBalance !== null && (
          <button
            type="button"
            onClick={() => navigate('/credits')}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm font-medium bg-blue-900/40 border border-blue-700/50 text-blue-200 hover:bg-blue-800/50 transition-colors"
          >
            <Coins className="h-4 w-4" />
            <span>{creditBalance.toLocaleString()}</span>
          </button>
        )}

        <button
          type="button"
          onClick={toggleLanguage}
          className="px-3 py-1 rounded-md text-sm font-medium border border-gray-600 text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
        >
          {locale === 'en' ? 'EN' : locale === 'tr' ? 'TR' : '中文'}
        </button>

        <button
          type="button"
          onClick={logout}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
        >
          <LogOut className="h-4 w-4" />
          <span className="hidden sm:inline">{t('auth.logout')}</span>
        </button>
      </div>
    </header>
  );
}
