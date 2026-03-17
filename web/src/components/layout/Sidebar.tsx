import { NavLink } from 'react-router-dom';
import {
  LayoutDashboard,
  MessageSquare,
  Wrench,
  Clock,
  Puzzle,
  Brain,
  Settings,
  DollarSign,
  Activity,
  Stethoscope,
} from 'lucide-react';
import { t } from '@/lib/i18n';

const navItems = [
  { to: '/', icon: LayoutDashboard, labelKey: 'nav.dashboard' },
  { to: '/agent', icon: MessageSquare, labelKey: 'nav.agent' },
  { to: '/tools', icon: Wrench, labelKey: 'nav.tools' },
  { to: '/cron', icon: Clock, labelKey: 'nav.cron' },
  { to: '/integrations', icon: Puzzle, labelKey: 'nav.integrations' },
  { to: '/memory', icon: Brain, labelKey: 'nav.memory' },
  { to: '/config', icon: Settings, labelKey: 'nav.config' },
  { to: '/cost', icon: DollarSign, labelKey: 'nav.cost' },
  { to: '/logs', icon: Activity, labelKey: 'nav.logs' },
  { to: '/doctor', icon: Stethoscope, labelKey: 'nav.doctor' },
];

export default function Sidebar() {
  return (
    <aside className="fixed top-0 left-0 h-screen w-60 flex flex-col sidebar">
      <div className="sidebar-glow-line" />

      <div className="flex items-center gap-3 px-4 py-4 border-b" style={{ borderColor: 'var(--color-border-default)' }}>
        <img
          src="/_app/logo.png"
          alt="ZeroClaw"
          className="h-10 w-10 rounded-xl object-cover animate-pulse-glow"
        />
        <span className="text-lg font-bold text-gradient-blue tracking-wide">
          ZeroClaw
        </span>
      </div>

      <nav className="flex-1 overflow-y-auto py-4 px-3 space-y-1">
        {navItems.map(({ to, icon: Icon, labelKey }, idx) => (
          <NavLink
            key={to}
            to={to}
            end={to === '/'}
            className={({ isActive }) =>
              [
                'flex items-center gap-3 px-3 py-2.5 rounded-xl text-sm font-medium transition-all duration-300 animate-slide-in-left group',
                isActive
                  ? 'sidebar-nav-item active'
                  : 'sidebar-nav-item',
              ].join(' ')
            }
            style={({ isActive }) => ({
              animationDelay: `${idx * 40}ms`,
              ...(isActive ? { background: 'var(--color-glow-blue)' } : {}),
            })}
          >
            {({ isActive }) => (
              <>
                <Icon className={`h-5 w-5 flex-shrink-0 transition-colors duration-300`} style={{ color: isActive ? 'var(--color-accent-blue)' : 'inherit' }} />
                <span>{t(labelKey)}</span>
                {isActive && (
                  <div className="ml-auto h-1.5 w-1.5 rounded-full" style={{ backgroundColor: 'var(--color-accent-blue)' }} />
                )}
              </>
            )}
          </NavLink>
        ))}
      </nav>

      <div className="px-5 py-4 border-t" style={{ borderColor: 'var(--color-border-default)' }}>
        <p className="text-xs tracking-wider uppercase" style={{ color: 'var(--color-text-muted)' }}>ZeroClaw Runtime</p>
      </div>
    </aside>
  );
}
