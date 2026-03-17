import { Outlet, useLocation } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';
import { ErrorBoundary } from '@/App';

export default function Layout() {
  const { pathname } = useLocation();

  return (
    <div className="min-h-screen page-background">
      <Sidebar />

      <div className="ml-60 flex flex-col min-h-screen">
        <Header />

        {/* Page content — ErrorBoundary keyed by pathname so the nav shell
            survives a page crash and the boundary resets on route change */}
        <main className="flex-1 overflow-y-auto">
          <ErrorBoundary key={pathname}>
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>
    </div>
  );
}
