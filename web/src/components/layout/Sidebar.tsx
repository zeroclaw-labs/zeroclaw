import { useEffect, useState } from 'react';
import { NavLink } from 'react-router-dom';
import {
  ChevronsLeftRightEllipsis,
  LayoutDashboard,
  MessageSquare,
  Wrench,
  Clock,
  Puzzle,
  Brain,
  Smartphone,
  Settings,
  DollarSign,
  Activity,
  Stethoscope,
  X,
} from 'lucide-react';
import { t } from '@/lib/i18n';

const COLLAPSE_BUTTON_DELAY_MS = 1000;

const navItems = [
  { to: '/', icon: LayoutDashboard, labelKey: 'nav.dashboard' },
  { to: '/agent', icon: MessageSquare, labelKey: 'nav.agent' },
  { to: '/tools', icon: Wrench, labelKey: 'nav.tools' },
  { to: '/cron', icon: Clock, labelKey: 'nav.cron' },
  { to: '/integrations', icon: Puzzle, labelKey: 'nav.integrations' },
  { to: '/memory', icon: Brain, labelKey: 'nav.memory' },
  { to: '/devices', icon: Smartphone, labelKey: 'nav.devices' },
  { to: '/config', icon: Settings, labelKey: 'nav.config' },
  { to: '/cost', icon: DollarSign, labelKey: 'nav.cost' },
  { to: '/logs', icon: Activity, labelKey: 'nav.logs' },
  { to: '/doctor', icon: Stethoscope, labelKey: 'nav.doctor' },
];

interface SidebarProps {
  isOpen: boolean;
  isCollapsed: boolean;
  onClose: () => void;
  onToggleCollapse: () => void;
}

export default function Sidebar({
  isOpen,
  isCollapsed,
  onClose,
  onToggleCollapse,
}: SidebarProps) {
  const [showCollapseButton, setShowCollapseButton] = useState(false);

  useEffect(() => {
    const id = setTimeout(() => setShowCollapseButton(true), COLLAPSE_BUTTON_DELAY_MS);
    return () => clearTimeout(id);
  }, []);

  return (
    <>
      <button
        type="button"
        aria-label="Close navigation"
        onClick={onClose}
        className={[
          'fixed inset-0 z-30 bg-black/50 transition-opacity md:hidden',
          isOpen ? 'opacity-100' : 'pointer-events-none opacity-0',
        ].join(' ')}
      />
      <aside
        className={[
          'fixed left-0 top-0 z-40 flex h-screen w-[86vw] max-w-[17.5rem] flex-col border-r border-[#1e2f5d] bg-[#050b1a]/95 backdrop-blur-xl',
          'shadow-[0_0_50px_-25px_rgba(8,121,255,0.7)]',
          'transform transition-[width,transform] duration-300 ease-out',
          isOpen ? 'translate-x-0' : '-translate-x-full',
          isCollapsed ? 'md:w-[6.25rem]' : 'md:w-[17.5rem]',
          'md:translate-x-0',
        ].join(' ')}
      >
        <div className="relative flex items-center justify-between border-b border-[#1a2d5e] px-4 py-4">
          <div className="flex items-center gap-3 overflow-hidden">
            {!isCollapsed && (
              <>
                <div
                  className="electric-brand-mark h-9 w-9 shrink-0 rounded-xl"
                  role="img"
                  aria-label="ZeroClaw"
                >
                  <span className="sr-only">ZeroClaw</span>
                </div>
                <span className="text-lg font-semibold tracking-[0.1em] text-white">
                  ZeroClaw
                </span>
              </>
            )}
          </div>

          <div className="flex items-center gap-2">
            {showCollapseButton && (
              <button
                type="button"
                onClick={onToggleCollapse}
                aria-label={isCollapsed ? 'Expand navigation' : 'Collapse navigation'}
                className="hidden rounded-lg border border-[#2c4e97] bg-[#0a1b3f]/60 p-1.5 text-[#8bb9ff] transition hover:border-[#4f83ff] hover:text-white md:block"
              >
                <ChevronsLeftRightEllipsis className="h-4 w-4" />
              </button>
            )}
            <button
              type="button"
              onClick={onClose}
              aria-label="Close navigation"
              className="rounded-lg p-1.5 text-gray-300 transition-colors hover:bg-gray-800 hover:text-white md:hidden"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>

        <nav className="flex-1 space-y-1 overflow-y-auto px-3 py-4">
          {navItems.map(({ to, icon: Icon, labelKey }) => (
            <NavLink
              key={to}
              to={to}
              end={to === '/'}
              onClick={onClose}
              title={isCollapsed ? t(labelKey) : undefined}
              className={({ isActive }) =>
                [
                  'group flex items-center gap-3 overflow-hidden rounded-xl px-3 py-2.5 text-sm font-medium transition-all duration-300',
                  isActive
                    ? 'border border-[#3a6de0] bg-[#0b2f80]/55 text-white shadow-[0_0_30px_-16px_rgba(72,140,255,0.95)]'
                    : 'border border-transparent text-[#9bb7eb] hover:border-[#294a8d] hover:bg-[#07132f] hover:text-white',
                ].join(' ')
              }
            >
              <Icon className="h-5 w-5 shrink-0 transition-transform duration-300 group-hover:scale-110" />
              <span
                className={[
                  'whitespace-nowrap transition-[opacity,transform,width] duration-300',
                  isCollapsed ? 'w-0 -translate-x-3 opacity-0 md:invisible' : 'w-auto opacity-100',
                ].join(' ')}
              >
                {t(labelKey)}
              </span>
            </NavLink>
          ))}
        </nav>

        <div
          className={[
            'mx-3 mb-4 rounded-xl border border-[#1b3670] bg-[#071328]/80 px-3 py-3 text-xs text-[#89a9df] transition-all duration-300',
            isCollapsed ? 'md:px-1.5 md:text-center' : '',
          ].join(' ')}
        >
          <p className={isCollapsed ? 'hidden md:block' : ''}>Gateway + Dashboard</p>
          <p className={isCollapsed ? 'text-[10px] uppercase tracking-widest' : 'mt-1 text-[#5f84cc]'}>
            {isCollapsed ? 'UI' : 'Runtime Mode'}
          </p>
        </div>
      </aside>
    </>
  );
}
