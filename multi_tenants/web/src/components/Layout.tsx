import { Link, useLocation } from 'react-router-dom';
import { useAuth } from '../hooks/useAuth';
import { LayoutDashboard, Server, Users, ScrollText, LogOut } from 'lucide-react';

const navItems = [
  { path: '/', label: 'Dashboard', admin: true, icon: LayoutDashboard },
  { path: '/tenants', label: 'Tenants', admin: false, icon: Server },
  { path: '/users', label: 'Users', admin: true, icon: Users },
  { path: '/audit', label: 'Audit Log', admin: true, icon: ScrollText },
];

export default function Layout({ children }: { children: React.ReactNode }) {
  const { email, isSuperAdmin, logout } = useAuth();
  const location = useLocation();

  return (
    <div className="flex min-h-screen bg-bg-primary">
      <aside className="w-60 bg-gray-900 text-white flex flex-col fixed inset-y-0 left-0 z-30">
        <div className="p-4 border-b border-border-default">
          <h2 className="text-lg font-bold tracking-tight">ZeroClaw</h2>
          <p className="text-xs text-text-muted truncate mt-0.5">{email}</p>
        </div>
        <nav className="flex-1 p-2 space-y-1">
          {navItems
            .filter(item => !item.admin || isSuperAdmin)
            .map(item => {
              const Icon = item.icon;
              const active = location.pathname === item.path;
              return (
                <Link
                  key={item.path}
                  to={item.path}
                  className={`flex items-center gap-3 px-3 py-2.5 rounded-lg text-sm font-medium transition-colors ${
                    active
                      ? 'bg-accent-blue/20 text-blue-400'
                      : 'text-gray-400 hover:bg-gray-800 hover:text-gray-200'
                  }`}
                >
                  <Icon className="h-4 w-4 shrink-0" />
                  {item.label}
                </Link>
              );
            })}
        </nav>
        <div className="p-2 border-t border-border-default">
          <button
            onClick={logout}
            className="w-full flex items-center gap-3 px-3 py-2.5 text-sm text-gray-400 hover:bg-gray-800 hover:text-gray-200 rounded-lg transition-colors text-left"
          >
            <LogOut className="h-4 w-4 shrink-0" />
            Logout
          </button>
        </div>
      </aside>
      <main className="flex-1 ml-60 p-6 overflow-auto">{children}</main>
    </div>
  );
}
