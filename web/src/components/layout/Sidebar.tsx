import { NavLink } from 'react-router-dom';
import { basePath } from '../../lib/basePath';
import {
  Activity,
  Clock,
  LayoutDashboard,
  MessageSquare,
  Monitor,
  Network,
  Puzzle,
  Settings,
  Stethoscope,
  Wrench,
  ArrowUpCircle,
  Loader2,
  CheckCircle2,
  AlertCircle,
} from 'lucide-react';
import { t } from '@/lib/i18n';
import { useEffect, useState } from 'react';
import { getStatus } from '@/lib/api';
import { useUpdate } from '@/hooks/useUpdate';
import type { UpdateState } from '@/hooks/useUpdate';

interface NavItem {
  to: string;
  icon: typeof LayoutDashboard;
  labelKey: string;
}

const navItems: NavItem[] = [
  { to: '/', icon: LayoutDashboard, labelKey: 'nav.dashboard' },
  { to: '/agents', icon: MessageSquare, labelKey: 'nav.agents' },
  { to: '/tools', icon: Wrench, labelKey: 'nav.tools' },
  { to: '/cron', icon: Clock, labelKey: 'nav.cron' },
  { to: '/integrations', icon: Puzzle, labelKey: 'nav.integrations' },
  { to: '/nodes', icon: Network, labelKey: 'nav.nodes' },
  { to: '/config', icon: Settings, labelKey: 'nav.config' },
  { to: '/logs', icon: Activity, labelKey: 'nav.logs' },
  { to: '/doctor', icon: Stethoscope, labelKey: 'nav.doctor' },
  { to: '/canvas', icon: Monitor, labelKey: 'nav.canvas' },
];

// The 6 onboarding sections (Workspace, Providers, Channels, Memory,
// Hardware, Tunnel) live under /config now — they're the first group
// inside the Config explorer's sidebar. The /setup/<section> deep-link
// route still works for bookmarks, but no top-level nav entries point
// at it. Run-setup-again link in /config covers the wizard re-entry.

// Shared nav item sub-component — eliminates duplication between mobile & desktop nav
function SidebarNavItem({ item, showLabel, showTooltip, onClick }: {
  item: NavItem;
  showLabel: boolean;
  showTooltip: boolean;
  onClick: () => void;
}) {
  const { to, icon: Icon, labelKey } = item;
  const text = t(labelKey);
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
          {showLabel && <span className="whitespace-nowrap">{text}</span>}
          {showTooltip && (
            <span
              className="absolute left-full ml-2 px-2 py-1 rounded-md text-xs whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none z-9999"
              style={{ background: 'var(--pc-bg-elevated)', color: 'var(--pc-text-primary)', border: '1px solid var(--pc-border)' }}
            >
              {text}
            </span>
          )}
        </>
      )}
    </NavLink>
  );
}

// Group header label — only shown when the sidebar is expanded. In the
// collapsed state we render a thin divider instead so the icons stay
// aligned and the separator is still discoverable.
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
  const [version, setVersion] = useState<string | null>(null);
  const update = useUpdate();

  useEffect(() => {
    getStatus()
      .then((s) => { if (s.version) setVersion(s.version); })
      .catch(() => { /* silently ignore */ });
  }, []);

  const renderUpdateButton = () => {
    if (collapsed) return null;

    switch (update.state) {
      case 'checking':
        return (
          <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[11px]" style={{ color: 'var(--pc-text-muted)' }}>
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            Checking...
          </div>
        );
      case 'available':
        return (
          <button
            onClick={update.run}
            className="w-full flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg text-[11px] font-medium transition-colors cursor-pointer"
            style={{ background: 'var(--pc-accent)', color: '#fff' }}
            title={`Update to v${update.latestVersion}`}
          >
            <ArrowUpCircle className="h-3.5 w-3.5" />
            Update to v{update.latestVersion}
          </button>
        );
      case 'updating':
        return (
          <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[11px]" style={{ color: 'var(--pc-text-muted)' }}>
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            <span className="truncate max-w-[140px]">{update.progressMsg}</span>
          </div>
        );
      case 'complete':
        return (
          <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[11px]" style={{ color: 'var(--color-status-success)' }}>
            <CheckCircle2 className="h-3.5 w-3.5" />
            Updated — restarting...
          </div>
        );
      case 'error':
        return (
          <div className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-[11px] cursor-pointer" style={{ color: 'var(--color-status-error)' }} onClick={update.check}>
            <AlertCircle className="h-3.5 w-3.5 shrink-0" />
            <span className="truncate max-w-30">{update.errorMsg}</span>
            <span style={{ color: 'var(--pc-accent)' }} className="underline ml-0.5">Retry</span>
          </div>
        );
      default:
        return (
          <button
            onClick={update.check}
            className="w-full flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg text-[11px] font-medium transition-colors cursor-pointer"
            style={{ background: 'var(--pc-accent-glow)', color: 'var(--pc-accent)', border: '1px solid var(--pc-accent-dim)' }}
            title="Check for updates"
          >
            <ArrowUpCircle className="h-3.5 w-3.5" />
            Check for updates
          </button>
        );
    }
  };

  if (layout === 'mobile') {
    return (
      <div
        className="px-5 py-4 border-t space-y-2"
        style={{ borderColor: 'var(--pc-border)' }}
      >
        {renderUpdateButton()}
        {version && (
          <div className="text-[12px] uppercase tracking-wider" style={{ color: 'var(--pc-text-faint)' }}>
            ZeroClaw Gateway
            <span className="ml-1 normal-case tracking-normal" style={{ fontSize: '11px' }}>v{version}</span>
          </div>
        )}
      </div>
    );
  }
  return (
    <div
      className="border-t shrink-0 whitespace-nowrap overflow-hidden transition-opacity duration-200"
      style={{
        borderColor: 'var(--pc-border)',
        padding: collapsed ? '12px 0' : '12px 16px',
        opacity: collapsed ? 0 : 1,
      }}
    >
      {!collapsed && renderUpdateButton()}
      {!collapsed && (
        <div
          className="mt-1.5 text-[12px] uppercase tracking-wider"
          style={{ color: 'var(--pc-text-faint)', paddingLeft: '4px' }}
        >
          ZeroClaw Gateway
          {version && (
            <span className="ml-1 normal-case tracking-normal" style={{ fontSize: '11px' }}>v{version}</span>
          )}
        </div>
      )}
    </div>
  );
}
