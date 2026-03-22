import { NavLink } from 'react-router-dom';
import { basePath } from '../../lib/basePath';
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
  Monitor,
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
  { to: '/canvas', icon: Monitor, labelKey: 'nav.canvas' },
];

export default function Sidebar() {
  return (
    <aside className="fixed top-0 left-0 h-screen w-60 flex flex-col border-r" style={{ background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}>
      {/* Logo / Title */}
      <div className="flex items-center gap-3 px-4 py-4 border-b h-14" style={{ borderColor: 'var(--pc-border)' }}>
        <div className="relative shrink-0">
          <div className="absolute -inset-1.5 rounded-xl" style={{ background: 'linear-gradient(135deg, rgba(var(--pc-accent-rgb), 0.15), rgba(var(--pc-accent-rgb), 0.05))' }} />
          <img
            src={`${basePath}/_app/zeroclaw-trans.png`}
            alt="ZeroClaw"
            className="relative h-9 w-9 rounded-xl object-cover"
            onError={(e) => {
              const img = e.currentTarget;
              img.style.display = 'none';
            }}
          />
        </div>
        <span className="text-sm font-semibold tracking-wide" style={{ color: 'var(--pc-text-primary)' }}>
          ZeroClaw
        </span>
      </div>

      {/* Navigation */}
      <nav className="flex-1 overflow-y-auto py-4 px-3 space-y-1">
        {navItems.map(({ to, icon: Icon, labelKey }, idx) => (
          <NavLink
            key={to}
            to={to}
            end={to === '/'}
            className={({ isActive }) =>
              [
                'flex items-center gap-3 px-3 py-2.5 rounded-2xl text-sm font-medium transition-all group',
                isActive
                  ? 'text-[var(--pc-accent-light)]'
                  : 'text-[var(--pc-text-muted)] hover:text-[var(--pc-text-secondary)] hover:bg-[var(--pc-hover)]',
              ].join(' ')
            }
            style={({ isActive }) => ({
              animationDelay: `${idx * 40}ms`,
              ...(isActive ? {
                background: 'var(--pc-accent-glow)',
                border: '1px solid var(--pc-accent-dim)',
              } : {}),
            })}
          >
            {({ isActive }) => (
              <>
                <Icon className={`h-5 w-5 flex-shrink-0 transition-colors ${isActive ? 'text-[var(--pc-accent)]' : 'group-hover:text-[var(--pc-accent)]'}`} />
                <span>{t(labelKey)}</span>
              </>
            )}
          </NavLink>
        ))}
      </nav>

      {/* Footer */}
      <div className="px-5 py-4 border-t text-[10px] uppercase tracking-wider" style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-faint)' }}>
        ZeroClaw Runtime
      </div>
    </aside>
  );
}
