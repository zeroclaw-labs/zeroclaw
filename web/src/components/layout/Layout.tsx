import { Outlet } from 'react-router-dom';
import { useState } from 'react';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';

const SIDEBAR_COLLAPSED_KEY = 'zeroclaw:sidebar-collapsed';

export default function Layout() {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState<boolean>(() => {
    if (typeof window === 'undefined') {
      return false;
    }
    try {
      return window.localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1';
    } catch {
      return false;
    }
  });

  const toggleSidebarCollapsed = () => {
    setSidebarCollapsed((prev) => {
      const next = !prev;
      if (typeof window !== 'undefined') {
        try {
          window.localStorage.setItem(SIDEBAR_COLLAPSED_KEY, next ? '1' : '0');
        } catch {
          // Storage unavailable, ignore
        }
      }
      return next;
    });
  };

  return (
    <div className="app-shell min-h-screen text-white">
      <Sidebar
        isOpen={sidebarOpen}
        isCollapsed={sidebarCollapsed}
        onClose={() => setSidebarOpen(false)}
        onToggleCollapse={toggleSidebarCollapsed}
      />

      <div
        className={[
          'flex min-h-screen flex-col transition-[margin-left] duration-300 ease-out',
          sidebarCollapsed ? 'md:ml-[6.25rem]' : 'md:ml-[17.5rem]',
        ].join(' ')}
      >
        <div className="relative z-50">
          <Header onToggleSidebar={() => setSidebarOpen((open) => !open)} />
        </div>

        <main className="relative z-10 flex-1 overflow-y-auto px-4 pb-8 pt-5 md:px-8 md:pt-8">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
