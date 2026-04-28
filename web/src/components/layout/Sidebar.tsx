import { NavLink } from 'react-router-dom';
import { basePath } from '../../lib/basePath';
import {
  LayoutDashboard,
  MessageSquare,
  Wrench,
  Clock,
  Puzzle,
  Brain,
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
  { to: '/cost', icon: DollarSign, labelKey: 'nav.cost' },
  { to: '/logs', icon: Activity, labelKey: 'nav.logs' },
  { to: '/doctor', icon: Stethoscope, labelKey: 'nav.doctor' },
  { to: '/canvas', icon: Monitor, labelKey: 'nav.canvas' },
];

// Shared nav item sub-component — eliminates duplication between mobile & desktop nav
function SidebarNavItem({ item, showLabel, showTooltip, onClick }: {
  item: (typeof navItems)[number];
  showLabel: boolean;
  showTooltip: boolean;
  onClick: () => void;
}) {
  const { to, icon: Icon, labelKey } = item;
  return (
    <NavLink
      key={to}
      to={to}
      end={to === '/'}
      onClick={onClick}
      className={({ isActive }) =>
        [
          'flex items-center rounded-xl text-sm font-medium transition-all group relative',
          showLabel ? 'justify-start gap-3 px-3 py-2.5' : 'justify-center w-10 h-10 mx-auto',
          isActive
            ? 'text-(--pc-accent-light)'
            : 'text-(--pc-text-muted) hover:text-(--pc-text-secondary) hover:bg-(--pc-hover)',
        ].join(' ')
      }
      style={({ isActive }) => ({
        ...(isActive ? { background: 'var(--pc-accent-glow)', border: '1px solid var(--pc-accent-dim)' } : {}),
      })}
    >
      {({ isActive }) => (
        <>
          <Icon className={`h-5 w-5 shrink-0 transition-colors ${isActive ? 'text-(--pc-accent)' : 'group-hover:text-(--pc-accent)'}`} />
          {showLabel && <span className="whitespace-nowrap">{t(labelKey)}</span>}
          {showTooltip && (
            <span
              className="absolute left-full ml-2 px-2 py-1 rounded-md text-xs whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none z-9999"
              style={{ background: 'var(--pc-bg-elevated)', color: 'var(--pc-text-primary)', border: '1px solid var(--pc-border)' }}
            >
              {t(labelKey)}
            </span>
          )}
        </>
      )}
    </NavLink>
  );
}

interface SidebarProps {
  open: boolean;
  onClose: () => void;
  collapsed: boolean;
}

export default function Sidebar({ open, onClose, collapsed }: SidebarProps) {
  return (
    <>
      {/* Backdrop — mobile only */}
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

      {/* Desktop sidebar — collapsible */}
      <aside
        className="hidden md:flex fixed top-0 left-0 h-screen flex-col border-r z-50 transition-all duration-300 ease-in-out"
        style={{ background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)', width: collapsed ? '56px' : '240px' }}
        aria-label={collapsed ? 'Collapsed sidebar' : 'Main sidebar'}
      >
        <SidebarLogo collapsed={collapsed} />
        <nav className="flex-1 overflow-y-auto py-4 px-2 space-y-1">
          {navItems.map((item) => (
            <SidebarNavItem
              key={item.to}
              item={item}
              showLabel={!collapsed}
              showTooltip={collapsed}
              onClick={onClose}
            />
          ))}
        </nav>
        <SidebarFooter collapsed={collapsed} layout="desktop" />
      </aside>

      {/* Mobile sidebar — slides in/out */}
      <aside
        className={[
          'md:hidden fixed top-0 left-0 h-screen w-60 flex flex-col border-r z-50 transition-transform duration-200 ease-out',
          open ? 'translate-x-0' : '-translate-x-full',
        ].join(' ')}
        style={{ background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}
        aria-label="Mobile menu"
      >
        <SidebarLogo collapsed={false} />
        <nav className="flex-1 overflow-y-auto py-4 px-3 space-y-1">
          {navItems.map((item) => (
            <SidebarNavItem
              key={item.to}
              item={item}
              showLabel
              showTooltip={false}
              onClick={onClose}
            />
          ))}
        </nav>
        <SidebarFooter collapsed={false} layout="mobile" />
      </aside>
    </>
  );
}

// Extracted sub-components to keep markup DRY

function SidebarLogo({ collapsed }: { collapsed: boolean }) {
  return (
    <div
      className="flex items-center border-b shrink-0 overflow-hidden"
      style={{
        borderColor: 'var(--pc-border)',
        height: '56px',
        padding: collapsed ? '0 14px' : '0 16px',
        gap: collapsed ? '0' : '12px',
      }}
    >
      <div className="relative shrink-0">
        <div className="absolute -inset-1.5 rounded-xl" style={{ background: 'linear-gradient(135deg, rgba(var(--pc-accent-rgb), 0.15), rgba(var(--pc-accent-rgb), 0.05))' }} />
        <img
          src={`${basePath}/_app/zeroclaw-trans.png`}
          alt="ZeroClaw"
          className="relative h-9 w-9 rounded-xl object-cover"
          onError={(e) => {
            e.currentTarget.style.display = 'none';
          }}
        />
      </div>
      <span
        className="text-sm font-semibold tracking-wide whitespace-nowrap transition-opacity duration-200"
        style={{
          color: 'var(--pc-text-primary)',
          opacity: collapsed ? 0 : 1,
          pointerEvents: collapsed ? 'none' : 'auto',
        }}
      >
        ZeroClaw
      </span>
    </div>
  );
}

function SidebarFooter({ collapsed, layout }: { collapsed: boolean; layout: 'desktop' | 'mobile' }) {
  if (layout === 'mobile') {
    return (
      <div
        className="px-5 py-4 border-t text-[10px] uppercase tracking-wider"
        style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-faint)' }}
      >
        ZeroClaw Runtime
      </div>
    );
  }
  return (
    <div
      className="border-t shrink-0 whitespace-nowrap overflow-hidden transition-opacity duration-200"
      style={{
        borderColor: 'var(--pc-border)',
        padding: collapsed ? '12px 0' : '16px 20px',
        fontSize: '10px',
        color: 'var(--pc-text-faint)',
        textTransform: 'uppercase',
        letterSpacing: '0.1em',
        opacity: collapsed ? 0 : 1,
        textAlign: collapsed ? 'center' : 'left',
      }}
    >
      {!collapsed && 'ZeroClaw Runtime'}
    </div>
  );
}
