import { NavLink } from 'react-router-dom';
import { basePath } from '../../lib/basePath';
import {
  Activity,
  ArrowDownToLine,
  Bot,
  Clock,
  LayoutDashboard,
  ListChecks,
  MessageSquare,
  Monitor,
  Puzzle,
  Settings,
  Smartphone,
  Sparkles,
  Stethoscope,
  Terminal,
  Workflow,
  Wrench,
} from 'lucide-react';
import { t } from '@/lib/i18n';
import { useEffect, useState } from 'react';
import { getStatus } from '@/lib/api';
import { useVersionCheck } from '@/hooks/useVersionCheck';
import { UpgradeDialog } from '@/components/UpgradeDialog';
import type { StatusResponse } from '@/types/api';

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
// is just organized under four clusters so the rail reads top-down by task:
// Home → Chat → Configure → Operations. On the desktop rail the cluster
// boundaries become thin divider rules (no text headings); the mobile drawer
// still renders the headings as full labels.
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
      { to: '/skills', icon: Sparkles, labelKey: 'nav.skills' },
      { to: '/sops', icon: Workflow, labelKey: 'nav.sops' },
      { to: '/runs', icon: ListChecks, labelKey: 'nav.runs' },
      { to: '/integrations', icon: Puzzle, labelKey: 'nav.integrations' },
      { to: '/cron', icon: Clock, labelKey: 'nav.cron' },
    ],
  },
  {
    headingKey: 'nav.group.operations',
    items: [
      { to: '/logs', icon: Activity, labelKey: 'nav.logs' },
      { to: '/pairing', icon: Smartphone, labelKey: 'nav.pairing' },
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

// ── Desktop rail item ───────────────────────────────────────────────────────
// Icon-only nav item for the slim rail. The icon is the affordance; the label
// is exposed three ways: title (native tooltip), aria-label (screen readers),
// and a token-styled popover to the right shown on hover OR keyboard focus.
// Active state = accent icon + a 2px left accent bar + subtle accent tint, with
// aria-current="page" so assistive tech announces the current section.
function RailNavItem({ item, onClick }: { item: NavItem; onClick: () => void }) {
  const { to, icon: Icon, labelKey } = item;
  const text = t(labelKey);
  return (
    <NavLink
      to={to}
      end={to === '/'}
      onClick={onClick}
      title={text}
      aria-label={text}
      className={({ isActive }) =>
        [
          'group relative flex h-10 w-10 mx-auto items-center justify-center',
          'rounded-[var(--radius-md)] transition-colors duration-150',
          'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]',
          isActive
            ? 'bg-pc-accent/10 text-pc-accent'
            : 'text-pc-text-muted hover:text-pc-text-secondary hover:bg-[var(--pc-hover)]',
        ].join(' ')
      }
    >
      {({ isActive }) => (
        <>
          {/* 2px left accent bar marking the active item against the rail edge. */}
          {isActive && (
            <span
              aria-hidden="true"
              className="absolute left-0 top-1.5 bottom-1.5 w-0.5 rounded-full bg-pc-accent"
            />
          )}
          <Icon
            className={`h-[22px] w-[22px] shrink-0 transition-colors ${
              isActive ? 'text-pc-accent' : 'group-hover:text-pc-text-secondary'
            }`}
          />
          {/* Tooltip popover to the right — appears on pointer hover and on
              keyboard focus (focus-within) so the rail is usable without a
              mouse. Token-styled; non-interactive so it never traps focus. */}
          <span
            role="tooltip"
            className="pointer-events-none absolute left-full ml-2 z-9999 whitespace-nowrap rounded-[var(--radius-sm)] px-2 py-1 text-xs opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
            style={{
              background: 'var(--pc-bg-elevated)',
              color: 'var(--pc-text-primary)',
              border: '1px solid var(--pc-border)',
            }}
          >
            {text}
          </span>
        </>
      )}
    </NavLink>
  );
}

// ── Mobile drawer item ──────────────────────────────────────────────────────
// Full labelled row (icon + text) for the mobile drawer, with the same calm
// active treatment as before: subtle accent tint, 2px left accent bar, accent
// icon, and aria-current via NavLink.
function DrawerNavItem({ item, onClick }: { item: NavItem; onClick: () => void }) {
  const { to, icon: Icon, labelKey } = item;
  const text = t(labelKey);
  return (
    <NavLink
      to={to}
      end={to === '/'}
      onClick={onClick}
      className={({ isActive }) =>
        [
          'group relative flex items-center justify-start gap-3 px-3 py-2',
          'rounded-[var(--radius-md)] text-sm font-medium transition-colors duration-150',
          isActive
            ? 'bg-pc-accent/10 text-pc-text'
            : 'text-pc-text-muted hover:text-pc-text-secondary hover:bg-[var(--pc-hover)]',
        ].join(' ')
      }
    >
      {({ isActive }) => (
        <>
          {isActive && (
            <span
              aria-hidden="true"
              className="absolute left-0 top-1.5 bottom-1.5 w-0.5 rounded-full bg-pc-accent"
            />
          )}
          <Icon
            className={`h-[22px] w-[22px] shrink-0 transition-colors ${
              isActive ? 'text-pc-accent' : 'group-hover:text-pc-text-secondary'
            }`}
          />
          <span className="whitespace-nowrap">{text}</span>
        </>
      )}
    </NavLink>
  );
}

// ── Mobile drawer group ─────────────────────────────────────────────────────
// One labelled cluster: a faint uppercase heading associated with its <ul> via
// aria-labelledby so screen readers announce the group name.
function DrawerGroup({ group, index, onClick }: {
  group: NavGroup;
  index: number;
  onClick: () => void;
}) {
  const heading = t(group.headingKey);
  const headingId = `nav-group-${index}`;
  return (
    <div role="group" aria-labelledby={headingId} className="space-y-0.5">
      <h2
        id={headingId}
        className="px-3 pt-3 pb-1 text-[10px] font-semibold uppercase tracking-wider select-none"
        style={{ color: 'var(--pc-text-faint)' }}
      >
        {heading}
      </h2>
      {group.items.map((item) => (
        <DrawerNavItem key={item.to} item={item} onClick={onClick} />
      ))}
    </div>
  );
}

interface SidebarProps {
  open: boolean;
  onClose: () => void;
}

export default function Sidebar({ open, onClose }: SidebarProps) {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  useEffect(() => {
    getStatus()
      .then(setStatus)
      .catch(() => { /* silently ignore */ });
  }, []);
  // `check_updates` is undefined on older gateways → treat as enabled.
  const checkUpdates = status?.check_updates !== false;
  const { info, loading, refetch } = useVersionCheck(checkUpdates);
  const hasUpdate = info?.is_newer === true;
  const version = status?.version ?? null;
  const [upgradeOpen, setUpgradeOpen] = useState(false);
  const openUpgrade = () => setUpgradeOpen(true);

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
          aria-label={t('sidebar.close_menu')}
        />
      )}

      {/* Desktop rail — permanent slim icon rail, always 56px. No collapse
          toggle: the rail is the navigation. Grouping is expressed as thin
          divider rules between the icon clusters. */}
      <aside
        className="hidden md:flex fixed top-0 left-0 h-screen w-14 flex-col border-r z-50"
        style={{ background: 'var(--pc-bg-sidebar)', borderColor: 'var(--pc-border)' }}
        aria-label={t('nav.aria.primary')}
      >
        <RailLogo />
        <nav className="flex-1 overflow-y-auto py-3 px-1.5" aria-label={t('nav.aria.primary')}>
          {navGroups.map((group, index) => (
            <div key={group.headingKey} className="space-y-1" role="group" aria-label={t(group.headingKey)}>
              {/* Thin divider between clusters (skipped before the first). */}
              {index > 0 && (
                <div
                  className="mx-auto my-2 h-px w-6"
                  style={{ background: 'var(--pc-separator)' }}
                  role="presentation"
                />
              )}
              {group.items.map((item) => (
                <RailNavItem key={item.to} item={item} onClick={onClose} />
              ))}
            </div>
          ))}
        </nav>
        <RailFooter version={version} hasUpdate={hasUpdate} onOpen={openUpgrade} />
      </aside>

      {/* Mobile drawer — labelled full version (icons + labels), slides in/out. */}
      <aside
        className={[
          'md:hidden fixed top-0 left-0 h-screen w-60 flex flex-col border-r z-50 transition-transform duration-200 ease-out',
          open ? 'translate-x-0' : '-translate-x-full',
        ].join(' ')}
        style={{ background: 'var(--pc-bg-sidebar)', borderColor: 'var(--pc-border)' }}
        aria-label={t('sidebar.mobile_menu')}
      >
        <DrawerLogo />
        <nav className="flex-1 overflow-y-auto py-3 px-2 space-y-0.5" aria-label={t('nav.aria.primary')}>
          {navGroups.map((group, index) => (
            <DrawerGroup key={group.headingKey} group={group} index={index} onClick={onClose} />
          ))}
        </nav>
        <DrawerFooter version={version} hasUpdate={hasUpdate} onOpen={openUpgrade} />
      </aside>

      <UpgradeDialog
        open={upgradeOpen}
        info={info}
        loading={loading}
        checkUpdatesEnabled={checkUpdates}
        allowSelfUpgrade={status?.allow_self_upgrade === true}
        restartMode={status?.restart_mode}
        restartHint={status?.restart_hint}
        onRefetch={refetch}
        onClose={() => setUpgradeOpen(false)}
      />
    </>
  );
}

// ── Logo / mark ─────────────────────────────────────────────────────────────

// Compact mark for the slim rail — the logo image only, centered, no wordmark.
function RailLogo() {
  return (
    <div
      className="flex items-center justify-center border-b shrink-0"
      style={{ borderColor: 'var(--pc-border)', height: '56px' }}
    >
      <div className="relative shrink-0">
        <div
          className="absolute -inset-1.5 rounded-xl"
          style={{ background: 'linear-gradient(135deg, rgba(var(--pc-accent-rgb), 0.15), rgba(var(--pc-accent-rgb), 0.05))' }}
        />
        <img
          src={`${basePath}/_app/logo.png`}
          alt={t('sidebar.logo_alt')}
          className="relative h-8 w-8 rounded-xl object-cover"
          onError={(e) => {
            e.currentTarget.style.display = 'none';
          }}
        />
      </div>
    </div>
  );
}

// Full mark + wordmark for the mobile drawer.
function DrawerLogo() {
  return (
    <div
      className="flex items-center border-b shrink-0 overflow-hidden"
      style={{ borderColor: 'var(--pc-border)', height: '56px', padding: '0 16px', gap: '12px' }}
    >
      <div className="relative shrink-0">
        <div
          className="absolute -inset-1.5 rounded-xl"
          style={{ background: 'linear-gradient(135deg, rgba(var(--pc-accent-rgb), 0.15), rgba(var(--pc-accent-rgb), 0.05))' }}
        />
        <img
          src={`${basePath}/_app/logo.png`}
          alt={t('sidebar.logo_alt')}
          className="relative h-9 w-9 rounded-xl object-cover"
          onError={(e) => {
            e.currentTarget.style.display = 'none';
          }}
        />
      </div>
      <span
        className="text-sm font-semibold tracking-wide whitespace-nowrap"
        style={{ color: 'var(--pc-text-primary)' }}
      >
        {t('sidebar.brand')}
      </span>
    </div>
  );
}

// ── Footers ─────────────────────────────────────────────────────────────────

interface FooterProps {
  version: string | null;
  /** A newer release is available — render an accent dot. */
  hasUpdate: boolean;
  /** Open the upgrade dialog. */
  onOpen: () => void;
}

// Rail footer — version tag as a button, centered, with a native tooltip
// carrying the full "ZeroClaw Gateway vX" string since the rail has no room for
// the label. When an update is available the version row is replaced by a
// pulsing download-arrow icon stacked above the version text — the dot was
// too easy to miss against the muted `text-faint` colour.
function RailFooter({ version, hasUpdate, onOpen }: FooterProps) {
  const title = hasUpdate
    ? t('sidebar.update_available')
    : version
      ? `${t('sidebar.gateway')} v${version}`
      : t('sidebar.gateway');
  return (
    <div
      className="border-t shrink-0 flex items-center justify-center"
      style={{ borderColor: 'var(--pc-border)', padding: '10px 0' }}
    >
      {(version || hasUpdate) && (
        <button
          type="button"
          onClick={onOpen}
          title={title}
          aria-label={title}
          className={[
            'relative flex flex-col items-center justify-center gap-0.5 rounded px-1.5 py-1 cursor-pointer transition-colors',
            hasUpdate
              ? 'bg-pc-accent/10 hover:bg-pc-accent/20'
              : 'hover:bg-pc-surface',
          ].join(' ')}
        >
          {hasUpdate && (
            <ArrowDownToLine
              aria-hidden="true"
              className="h-3.5 w-3.5 animate-bounce-soft"
              style={{ color: 'var(--pc-accent)' }}
            />
          )}
          {/* Red badge dot — an unmistakable attention-grabber layered atop the
              pulsing arrow so an available update is never missed at a glance. */}
          {hasUpdate && (
            <span
              aria-hidden="true"
              className="absolute rounded-full"
              style={{
                top: '2px',
                right: '2px',
                width: '7px',
                height: '7px',
                backgroundColor: 'var(--color-status-error)',
                boxShadow: '0 0 0 1.5px var(--pc-bg-surface)',
              }}
            />
          )}
          <span
            style={{
              fontSize: '9px',
              color: hasUpdate ? 'var(--pc-accent)' : 'var(--pc-text-faint)',
              fontWeight: hasUpdate ? 600 : undefined,
            }}
          >
            {version ? `v${version}` : t('upgrade.title')}
          </span>
        </button>
      )}
    </div>
  );
}

// Drawer footer — full labelled gateway line for mobile, clickable to upgrade.
// When an update is available the dot is replaced by a soft-bouncing download
// arrow rendered at body-text size so the affordance is unmistakable.
function DrawerFooter({ version, hasUpdate, onOpen }: FooterProps) {
  return (
    <div
      className="px-5 py-4 border-t text-[10px] uppercase tracking-wider"
      style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-faint)' }}
    >
      <button
        type="button"
        onClick={onOpen}
        title={hasUpdate ? t('sidebar.update_available') : undefined}
        className={[
          'flex items-center gap-1.5 cursor-pointer transition-opacity uppercase tracking-wider',
          hasUpdate ? 'opacity-100' : 'hover:opacity-80',
        ].join(' ')}
        style={hasUpdate ? { color: 'var(--pc-accent)' } : undefined}
      >
        {hasUpdate && (
          <ArrowDownToLine
            aria-hidden="true"
            className="h-3.5 w-3.5 animate-bounce-soft"
          />
        )}
        <span>
          {hasUpdate ? t('sidebar.update_available') : t('sidebar.gateway')}
        </span>
      </button>
      {version && (
        <div className="mt-0.5 normal-case tracking-normal" style={{ fontSize: '9px' }}>
          v{version}
        </div>
      )}
    </div>
  );
}
