import { useEffect, useRef } from 'react';
import type { ReactNode, ComponentType } from 'react';
import { Link } from 'react-router-dom';
import {
  BookOpen,
  Bot,
  Brain,
  Clock,
  Database,
  DollarSign,
  MessageSquare,
  Pencil,
  Plug,
  Power,
  Shield,
  Sparkles,
  Users,
  Wifi,
  X,
  Zap,
} from 'lucide-react';
import type { LucideProps } from 'lucide-react';
import type { AgentSummary } from '@/lib/agents';
import { Badge } from '@/components/ui';
import EntityLink from './EntityLink';

export interface AgentDrawerProps {
  /** The agent to show. When null the drawer is closed (renders nothing). */
  agent: AgentSummary | null;
  /** Clear the selection / close the drawer. */
  onClose: () => void;
  /** Flip the agent's enabled flag (same handler the list rows use). */
  onToggle: (agent: AgentSummary) => void;
  /** Whether this agent's toggle request is in flight. */
  toggling: boolean;
}

function formatRelative(iso: string | null): string {
  if (!iso) return 'no sessions yet';
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return 'no sessions yet';
  const diffSec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (diffSec < 60) return 'just now';
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86_400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86_400)}d ago`;
}

function formatUsd(value: number | null): string {
  if (value === null) return '—';
  if (value < 0.01) return '<$0.01';
  return `$${value.toFixed(2)}`;
}

// Calm chip mirroring the list-row treatment: a muted token surface that links
// into config.
const CHIP_CLASS =
  'inline-block font-mono text-[10px] px-2 py-0.5 rounded-full ' +
  'bg-pc-elevated text-pc-text-secondary hover:text-pc-text transition-colors';

const ACTION_BASE =
  'inline-flex items-center justify-center gap-1.5 h-9 px-3.5 text-sm ' +
  'font-medium whitespace-nowrap rounded-[var(--radius-md)] border ' +
  'transition-colors duration-150 select-none ' +
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] ' +
  'focus-visible:ring-offset-2 focus-visible:ring-offset-pc-surface';

// A labelled group: a muted caption + icon over a wrapped set of facts. Reused
// for each config dimension so the drawer reads as scannable sections.
function DetailGroup({
  icon: Icon,
  label,
  children,
}: {
  icon: ComponentType<LucideProps>;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <span className="flex items-center gap-1.5 text-[11px] uppercase tracking-wide text-pc-text-faint">
        <Icon className="h-3 w-3 flex-shrink-0" />
        {label}
      </span>
      <div className="flex flex-wrap items-center gap-1.5 text-sm text-pc-text-secondary">
        {children}
      </div>
    </div>
  );
}

export default function AgentDrawer({
  agent,
  onClose,
  onToggle,
  toggling,
}: AgentDrawerProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const closeBtnRef = useRef<HTMLButtonElement>(null);

  const open = agent !== null;

  // Focus the close button on open; restore focus to the previously-focused
  // element (the row that opened the drawer) on close.
  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    closeBtnRef.current?.focus();
    return () => previouslyFocused?.focus?.();
  }, [open]);

  // Esc closes; Tab is trapped inside the drawer panel.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
        return;
      }
      if (e.key !== 'Tab') return;
      const panel = panelRef.current;
      if (!panel) return;
      const focusable = panel.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      const active = document.activeElement;
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!agent) return null;

  const channelCount = agent.channels.length;
  const skillCount = agent.skillBundles.length;
  const knowledgeCount = agent.knowledgeBundles.length;
  const mcpCount = agent.mcpBundles.length;
  const cronCount = agent.cronJobs.length;
  const peerCount = agent.peerGroups.length;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={`Agent ${agent.alias} detail`}
      className="fixed inset-0 z-50 flex justify-end"
      onClick={onClose}
    >
      {/* Backdrop */}
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />

      {/* Panel: full-screen on mobile, right-side drawer on >= sm. */}
      <div
        ref={panelRef}
        className="relative h-full w-full sm:max-w-md flex flex-col bg-pc-base border-l border-pc-border shadow-[var(--pc-shadow-md)] animate-slide-in-right overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header: identity + close */}
        <div className="flex items-start justify-between gap-3 px-5 py-4 border-b border-pc-border">
          <div className="flex items-center gap-3 min-w-0">
            <div className="h-10 w-10 rounded-[var(--radius-md)] flex-shrink-0 flex items-center justify-center bg-pc-accent/10">
              <Bot className="h-5 w-5 text-pc-accent" />
            </div>
            <div className="min-w-0">
              <EntityLink
                kind="agent"
                id={agent.alias}
                className="block text-base font-semibold truncate text-pc-text hover:underline"
                title={`Open agents.${agent.alias} config`}
              >
                {agent.alias}
              </EntityLink>
              {agent.modelProvider ? (
                <EntityLink
                  kind="model-provider"
                  id={agent.modelProvider}
                  className="block text-xs truncate font-mono text-pc-text-muted hover:text-pc-text-secondary hover:underline"
                  title={`Open providers.models.${agent.modelProvider} config`}
                >
                  {agent.modelProvider}
                </EntityLink>
              ) : (
                <p className="text-xs truncate text-pc-text-muted">
                  no model_provider set
                </p>
              )}
            </div>
          </div>
          <button
            ref={closeBtnRef}
            type="button"
            onClick={onClose}
            aria-label="Close"
            title="Close"
            className="h-8 w-8 flex-shrink-0 rounded-[var(--radius-md)] flex items-center justify-center text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Scrollable body */}
        <div className="flex-1 overflow-y-auto px-5 py-4 flex flex-col gap-5">
          {/* Status */}
          <div className="flex items-center justify-between gap-3">
            <span className="text-[11px] uppercase tracking-wide text-pc-text-faint">
              Status
            </span>
            <button
              type="button"
              onClick={() => onToggle(agent)}
              disabled={toggling}
              className="rounded-full transition-opacity disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
              aria-pressed={agent.enabled}
              aria-label={agent.enabled ? 'Disable agent' : 'Enable agent'}
              title={agent.enabled ? 'Disable agent' : 'Enable agent'}
            >
              <Badge tone={agent.enabled ? 'ok' : 'neutral'}>
                <Power className="h-3 w-3" />
                {agent.enabled ? 'enabled' : 'disabled'}
              </Badge>
            </button>
          </div>

          {/* Configuration facts */}
          <DetailGroup icon={Wifi} label="Channels">
            {channelCount === 0 ? (
              <span className="text-pc-text-muted">none bound</span>
            ) : (
              agent.channels.map((ch) => (
                <EntityLink
                  key={ch}
                  kind="channel"
                  id={ch}
                  className={CHIP_CLASS}
                  title={`Open channels.${ch} config`}
                >
                  {ch}
                </EntityLink>
              ))
            )}
          </DetailGroup>

          <DetailGroup icon={Shield} label="Profile">
            {agent.riskProfile ? (
              <EntityLink
                kind="risk-profile"
                id={agent.riskProfile}
                className="inline-flex items-center gap-1 hover:text-pc-text hover:underline"
                title="Risk profile (autonomy/sandbox tier)"
              >
                {agent.riskProfile}
              </EntityLink>
            ) : (
              <span
                className="text-pc-text-muted"
                title="Risk profile (autonomy/sandbox tier)"
              >
                no risk profile
              </span>
            )}
            <span className="text-pc-text-faint">·</span>
            <EntityLink
              kind="memory-backend"
              id=""
              className="inline-flex items-center gap-1 hover:text-pc-text hover:underline"
              title={
                agent.memoryBackend
                  ? `Memory backend: ${agent.memoryBackend}`
                  : 'No per-agent override. Inherits the default backend (sqlite) from [memory].'
              }
            >
              <Database className="h-3 w-3 flex-shrink-0" />
              {agent.memoryBackend || 'sqlite (default)'}
            </EntityLink>
            {agent.runtimeProfile && (
              <>
                <span className="text-pc-text-faint">·</span>
                <EntityLink
                  kind="runtime-profile"
                  id={agent.runtimeProfile}
                  className="inline-flex items-center gap-1 hover:text-pc-text hover:underline"
                  title="Runtime profile (loop, token limits, retries)"
                >
                  <Zap className="h-3 w-3 flex-shrink-0" />
                  {agent.runtimeProfile}
                </EntityLink>
              </>
            )}
          </DetailGroup>

          {skillCount > 0 && (
            <DetailGroup icon={Sparkles} label="Skills">
              {agent.skillBundles.map((s) => (
                <EntityLink
                  key={s}
                  kind="skill-bundle"
                  id={s}
                  className={CHIP_CLASS}
                  title={`Open skill-bundles.${s} config`}
                >
                  {s}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {knowledgeCount > 0 && (
            <DetailGroup icon={BookOpen} label="Knowledge">
              {agent.knowledgeBundles.map((k) => (
                <EntityLink
                  key={k}
                  kind="knowledge-bundle"
                  id={k}
                  className={CHIP_CLASS}
                  title={`Open knowledge-bundles.${k} config`}
                >
                  {k}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {mcpCount > 0 && (
            <DetailGroup icon={Plug} label="MCP">
              {agent.mcpBundles.map((m) => (
                <EntityLink
                  key={m}
                  kind="mcp-bundle"
                  id={m}
                  className={CHIP_CLASS}
                  title={`Open mcp-bundles.${m} config`}
                >
                  {m}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {peerCount > 0 && (
            <DetailGroup icon={Users} label="Peers">
              {agent.peerGroups.map((pg) => (
                <EntityLink
                  key={pg}
                  kind="peer-group"
                  id={pg}
                  className={CHIP_CLASS}
                  title={`Open peer_groups.${pg} config`}
                >
                  {pg}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {cronCount > 0 && (
            <DetailGroup icon={Clock} label="Cron">
              {agent.cronJobs.map((c) => (
                <EntityLink
                  key={c}
                  kind="cron"
                  id={c}
                  className={CHIP_CLASS}
                  title={`Open cron.${c} config`}
                >
                  {c}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {/* Activity stats: sessions / memories / spend */}
          <div className="grid grid-cols-3 gap-2 pt-4 border-t border-pc-border">
            <div className="min-w-0">
              <div className="flex items-center gap-1 text-[11px] text-pc-text-faint">
                <MessageSquare className="h-3 w-3 flex-shrink-0" />
                Sessions
              </div>
              <div className="mt-0.5 text-sm text-pc-text">
                {agent.sessionCount === 0 ? (
                  <span className="text-pc-text-muted">none</span>
                ) : (
                  <Link
                    to={`/?tab=sessions&agent=${encodeURIComponent(agent.alias)}`}
                    className="hover:text-pc-accent hover:underline"
                    title={`Show sessions for ${agent.alias}`}
                  >
                    {agent.sessionCount}
                  </Link>
                )}
              </div>
              <div className="text-[11px] text-pc-text-muted truncate">
                {formatRelative(agent.lastActivity)}
              </div>
            </div>

            <div className="min-w-0">
              <div className="flex items-center gap-1 text-[11px] text-pc-text-faint">
                <Brain className="h-3 w-3 flex-shrink-0" />
                Memories
              </div>
              <div className="mt-0.5 text-sm text-pc-text">
                {agent.memoryCount === 0 ? (
                  <span className="text-pc-text-muted">none</span>
                ) : (
                  <Link
                    to={`/?tab=memories&agent=${encodeURIComponent(agent.alias)}`}
                    className="hover:text-pc-accent hover:underline"
                    title={`Show memories for ${agent.alias}`}
                  >
                    {agent.memoryCount}
                  </Link>
                )}
              </div>
            </div>

            <div
              className="min-w-0"
              title={
                agent.monthCostUsd === null
                  ? 'Per-agent tracking disabled in [cost].track_per_agent'
                  : 'Month-to-date spend attributed to this agent'
              }
            >
              <div className="flex items-center gap-1 text-[11px] text-pc-text-faint">
                <DollarSign className="h-3 w-3 flex-shrink-0" />
                This month
              </div>
              <div className="mt-0.5 text-sm text-pc-text">
                {formatUsd(agent.monthCostUsd)}
              </div>
            </div>
          </div>
        </div>

        {/* Sticky footer actions. Routes are <Link>s styled to match the Button
            primitive (Button renders a native <button>, so it can't navigate). */}
        <div className="flex items-center gap-2 px-5 py-4 border-t border-pc-border">
          <Link
            to={`/agent/${encodeURIComponent(agent.alias)}`}
            className={`${ACTION_BASE} flex-1 bg-pc-accent border-transparent text-[#0b1220] hover:bg-pc-accent-light active:brightness-95`}
          >
            <MessageSquare className="h-4 w-4" />
            Open chat
          </Link>
          <Link
            to={`/config/agents/${encodeURIComponent(agent.alias)}`}
            className={`${ACTION_BASE} bg-transparent border-pc-border text-pc-text-secondary hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong`}
          >
            <Pencil className="h-4 w-4" />
            Edit
          </Link>
        </div>
      </div>
    </div>
  );
}
