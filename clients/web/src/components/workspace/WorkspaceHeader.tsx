'use client';

import { usePathname } from 'next/navigation';
import { LogOut, Menu } from 'lucide-react';
import { t } from '@/lib/i18n';
import { useAuth } from '@/hooks/useAuth';

const routeTitles: Record<string, string> = {
  '/workspace/dashboard': 'nav.dashboard',
  '/chat': 'nav.agent',
  '/workspace/tools': 'nav.tools',
  '/workspace/cron': 'nav.cron',
  '/workspace/integrations': 'nav.integrations',
  '/workspace/memory': 'nav.memory',
  '/workspace/devices': 'nav.devices',
  '/workspace/config': 'nav.config',
  '/workspace/cost': 'nav.cost',
  '/workspace/logs': 'nav.logs',
  '/workspace/doctor': 'nav.doctor',
};

interface WorkspaceHeaderProps {
  onToggleSidebar: () => void;
}

export default function WorkspaceHeader({ onToggleSidebar }: WorkspaceHeaderProps) {
  const pathname = usePathname();
  const { logout } = useAuth();

  const titleKey = routeTitles[pathname] ?? 'nav.dashboard';
  const pageTitle = t(titleKey);

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
