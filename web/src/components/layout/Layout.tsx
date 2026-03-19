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
      <div className="ml-60 flex flex-col min-h-screen">
        <Header />

        {/* Page content */}
        <main className="flex-1 overflow-y-auto">
          <ErrorBoundary key={pathname}>
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>
    </div>
  );
}
