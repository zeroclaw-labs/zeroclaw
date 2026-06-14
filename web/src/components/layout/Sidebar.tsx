import { NavLink } from 'react-router-dom';
import { basePath } from '../../lib/basePath';
import {
  Activity,
  Bot,
  Clock,
  LayoutDashboard,
  MessageSquare,
  Monitor,
  Puzzle,
  Settings,
  Stethoscope,
  Terminal,
  Wrench,
} from 'lucide-react';
import { t } from '@/lib/i18n';
import { useEffect, useState } from 'react';
import { getStatus } from '@/lib/api';

interface NavItem {
  to: string;
  icon: typeof LayoutDashboard;
  labelKey: string;
}

interface NavGroup {
  headingKey: string;
  items: NavItem[];
}

// Grouped navigation. Every existing route/link is preserved — the flat list
// is just organized under four headings so the sidebar reads top-down by task:
// Home → Chat → Configure → Operations.
const navGroups: NavGroup[] = [
  {
    headingKey: 'nav.group.home',
    items: [{ to: '/', icon: LayoutDashboard, labelKey: 'nav.dashboard' }],
  },
  {
    headingKey: 'nav.group.chat',
    items: [{ to: '/agents', icon: MessageSquare, labelKey: 'nav.agents' }],
  },
  {
    headingKey: 'nav.group.configure',
    items: [
      { to: '/config', icon: Settings, labelKey: 'nav.config' },
      { to: '/config/agents', icon: Bot, labelKey: 'nav.agent' },
      { to: '/tools', icon: Wrench, labelKey: 'nav.tools' },
      { to: '/integrations', icon: Puzzle, labelKey: 'nav.integrations' },
      { to: '/cron', icon: Clock, labelKey: 'nav.cron' },
    ],
  },
  {
    headingKey: 'nav.group.operations',
    items: [
      { to: '/logs', icon: Activity, labelKey: 'nav.logs' },
      { to: '/doctor', icon: Stethoscope, labelKey: 'nav.doctor' },
      { to: '/canvas', icon: Monitor, labelKey: 'nav.canvas' },
      { to: '/acp-console', icon: Terminal, labelKey: 'nav.acp' },
    ],
  },
];

// The 6 Quickstart sections (Workspace, Providers, Channels, Memory,
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
          // Calm console row: subtle accent tint + a 2px left accent bar when
          // active (the bar/icon carry the accent; the label stays primary
          // text so the row reads quiet rather than as a bright filled pill).
          'flex items-center rounded-[var(--radius-md)] text-sm font-medium transition-colors duration-150 group relative',
          showLabel ? 'justify-start gap-3 px-3 py-2' : 'justify-center w-10 h-10 mx-auto',
          isActive
            ? 'bg-pc-accent/10 text-pc-text'
            : 'text-pc-text-muted hover:text-pc-text-secondary hover:bg-[var(--pc-hover)]',
        ].join(' ')
      }
    >
      {({ isActive }) => (
        <>
          {/* 2px left accent bar — only on the expanded (labelled) layout so it
              doesn't crowd the centered collapsed icons. */}
          {isActive && showLabel && (
            <span
              aria-hidden="true"
              className="absolute left-0 top-1.5 bottom-1.5 w-0.5 rounded-full bg-pc-accent"
            />
          )}
          <Icon
            className={`h-[18px] w-[18px] shrink-0 transition-colors ${
              isActive ? 'text-pc-accent' : 'group-hover:text-pc-text-secondary'
            }`}
          />
          {showLabel && <span className="whitespace-nowrap">{text}</span>}
          {showTooltip && (
            <span
              className="absolute left-full ml-2 px-2 py-1 rounded-[var(--radius-sm)] text-xs whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none z-9999"
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

// Renders one nav group: a heading (or a thin divider when collapsed) plus its
// items. The heading is associated with its <ul> via aria-labelledby so screen
// readers announce the group name. The leading group renders without a top
// divider so the list doesn't open with a stray rule.
function SidebarGroup({ group, index, showLabel, showTooltip, onClick }: {
  group: NavGroup;
  index: number;
  showLabel: boolean;
  showTooltip: boolean;
  onClick: () => void;
}) {
  const heading = t(group.headingKey);
  const headingId = `nav-group-${index}`;
  return (
    <div role="group" aria-labelledby={showLabel ? headingId : undefined} className="space-y-0.5">
      {showLabel ? (
        <h2
          id={headingId}
          className="px-3 pt-3 pb-1 text-[10px] font-semibold uppercase tracking-wider select-none"
          style={{ color: 'var(--pc-text-faint)' }}
        >
          {heading}
        </h2>
      ) : (
        index > 0 && (
          <div
            className="mx-auto my-2 h-px w-6"
            style={{ background: 'var(--pc-separator)' }}
            role="presentation"
            aria-label={heading}
          />
        )
      )}
      {group.items.map((item) => (
        <SidebarNavItem
          key={item.to}
          item={item}
          showLabel={showLabel}
          showTooltip={showTooltip}
          onClick={onClick}
        />
      ))}
    </div>
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
        style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)', width: collapsed ? '56px' : '240px' }}
        aria-label={collapsed ? 'Collapsed sidebar' : 'Main sidebar'}
      >
        <SidebarLogo collapsed={collapsed} />
        <nav className="flex-1 overflow-y-auto py-3 px-2 space-y-0.5" aria-label={t('nav.aria.primary')}>
          {navGroups.map((group, index) => (
            <SidebarGroup
              key={group.headingKey}
              group={group}
              index={index}
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
        style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)' }}
        aria-label="Mobile menu"
      >
        <SidebarLogo collapsed={false} />
        <nav className="flex-1 overflow-y-auto py-3 px-2 space-y-0.5" aria-label={t('nav.aria.primary')}>
          {navGroups.map((group, index) => (
            <SidebarGroup
              key={group.headingKey}
              group={group}
              index={index}
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

  useEffect(() => {
    getStatus()
      .then((s) => { if (s.version) setVersion(s.version); })
      .catch(() => { /* silently ignore */ });
  }, []);

  if (layout === 'mobile') {
    return (
      <div
        className="px-5 py-4 border-t text-[10px] uppercase tracking-wider"
        style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-faint)' }}
      >
        ZeroClaw Gateway
        {version && (
          <div className="mt-0.5 normal-case tracking-normal" style={{ fontSize: '9px' }}>
            v{version}
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
        padding: collapsed ? '12px 0' : '16px 20px',
        fontSize: '10px',
        color: 'var(--pc-text-faint)',
        textTransform: 'uppercase',
        letterSpacing: '0.1em',
        opacity: collapsed ? 0 : 1,
        textAlign: collapsed ? 'center' : 'left',
      }}
    >
      {!collapsed && 'ZeroClaw Gateway'}
      {!collapsed && version && (
        <div style={{ marginTop: '2px', fontSize: '9px', textTransform: 'none', letterSpacing: 'normal' }}>
          v{version}
        </div>
      )}
    </div>
  );
}
