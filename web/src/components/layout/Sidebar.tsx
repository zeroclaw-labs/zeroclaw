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

interface SidebarProps {
  open: boolean;
  onClose: () => void;
}

export default function Sidebar({ open, onClose }: SidebarProps) {
  return (
    <>
      {/* Backdrop — mobile only, visible when sidebar is open */}
      {open && (
        <div
          className="md:hidden fixed inset-0 z-40 bg-black/60 backdrop-blur-sm transition-opacity"
          onClick={onClose}
          onKeyDown={(e) => { if (e.key === 'Escape') onClose(); }}
          role="button"
          tabIndex={-1}
          aria-label="Close menu"
        />
      )}

      <aside
        className={[
          'fixed top-0 left-0 h-screen w-60 flex flex-col z-50',
          // Mobile: slide in/out with transition
          'max-md:-translate-x-full max-md:transition-transform max-md:duration-200 max-md:ease-out',
          open ? 'max-md:translate-x-0' : '',
        ].join(' ')}
        style={{ background: 'linear-gradient(180deg, #080818 0%, #050510 100%)' }}
      >
        {/* Glow line on right edge */}
        <div className="sidebar-glow-line" />

        {/* Logo / Title */}
        <div className="flex items-center gap-3 px-4 py-4 border-b border-[#1a1a3e]/50">
          <img
            src="/_app/zeroclaw-trans.png"
            alt="ZeroClaw"
            className="h-10 w-10 rounded-xl object-cover animate-pulse-glow"
          />
          <span className="text-lg font-bold text-gradient-blue tracking-wide">
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
              onClick={onClose}
              className={({ isActive }) =>
                [
                  'flex items-center gap-3 px-3 py-2.5 rounded-xl text-sm font-medium transition-all duration-300 animate-slide-in-left group',
                  isActive
                    ? 'text-white shadow-[0_0_15px_rgba(0,128,255,0.2)]'
                    : 'text-[#556080] hover:text-white hover:bg-[#0080ff08]',
                ].join(' ')
              }
              style={({ isActive }) => ({
                animationDelay: `${idx * 40}ms`,
                ...(isActive ? { background: 'linear-gradient(135deg, rgba(0,128,255,0.15), rgba(0,128,255,0.05))' } : {}),
              })}
            >
              {({ isActive }) => (
                <>
                  <Icon className={`h-5 w-5 flex-shrink-0 transition-colors duration-300 ${isActive ? 'text-[#0080ff]' : 'group-hover:text-[#0080ff80]'}`} />
                  <span>{t(labelKey)}</span>
                  {isActive && (
                    <div className="ml-auto h-1.5 w-1.5 rounded-full bg-[#0080ff] glow-dot" />
                  )}
                </>
              )}
            </NavLink>
          ))}
        </nav>

        {/* Footer */}
        <div className="px-5 py-4 border-t border-[#1a1a3e]/50">
          <p className="text-[10px] text-[#334060] tracking-wider uppercase">ZeroClaw Runtime</p>
        </div>
      </aside>
    </>
  );
}
