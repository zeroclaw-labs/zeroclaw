import { Outlet, useLocation } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';
import { ErrorBoundary } from '@/App';

export default function Layout() {
  const { pathname } = useLocation();

  return (
    <div className="min-h-screen text-white" style={{ background: 'var(--pc-bg-base)' }}>
      {/* Fixed sidebar */}
      <Sidebar />

      {/* Main area offset by sidebar width (240px / w-60) */}
      <div className="ml-60 flex flex-col flex-1 min-w-0 h-screen">
        <Header />

        {/* Page content — ErrorBoundary keyed by pathname so the nav shell
            survives a page crash and the boundary resets on route change */}
        <main className="flex-1 overflow-y-auto min-h-0">
          <ErrorBoundary key={pathname}>
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>
    </div>
  );
}
