import { Outlet } from 'react-router-dom';
import Sidebar from '@/components/layout/Sidebar';
import Header from '@/components/layout/Header';

export default function Layout() {
  return (
    <div className="min-h-screen text-white" style={{ background: 'linear-gradient(135deg, #050510 0%, #080818 50%, #050510 100%)' }}>
      {/* Fixed sidebar */}
      <Sidebar />

      {/* Main area offset by sidebar width (240px / w-60) */}
      <div className="ml-60 flex flex-col min-h-screen">
        <Header />

        {/* Page content */}
        <main className="flex-1 overflow-y-auto">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
