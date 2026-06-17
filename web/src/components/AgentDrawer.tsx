import { useEffect, useRef } from 'react';
import type { ReactNode, ComponentType } from 'react';
import { Link } from 'react-router-dom';
import {
  useFocusTrap,
  FOCUSABLE_SELECTOR_FORM,
} from '@/hooks/useFocusTrap';
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
import { t } from '@/lib/i18n';
import { formatRelative, formatUsd } from '@/lib/format';
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

  // Esc closes; Tab is trapped inside the drawer panel; focus is restored to the
  // opener (the row that opened the drawer) on close. Matches the prior
  // hand-rolled effect (wide selector that includes select/textarea, no
  // visibility filter, Esc does not preventDefault). Declared before the
  // focus-on-open effect so the trap captures the opener as the restore target
  // before focus moves into the panel.
  useFocusTrap(panelRef, {
    onClose,
    enabled: open,
    focusableSelector: FOCUSABLE_SELECTOR_FORM,
  });

  // Focus the close button on open. (Store/restore + Esc/Tab handled above.)
  useEffect(() => {
    if (!open) return;
    closeBtnRef.current?.focus();
  }, [open]);

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
      aria-label={`${t('agent.detail_aria_prefix')} ${agent.alias} ${t('agent.detail_aria_suffix')}`}
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
                title={`${t('agent.open_config_prefix')}agents.${agent.alias}${t('agent.open_config_suffix')}`}
              >
                {agent.alias}
              </EntityLink>
              {agent.modelProvider ? (
                <EntityLink
                  kind="model-provider"
                  id={agent.modelProvider}
                  className="block text-xs truncate font-mono text-pc-text-muted hover:text-pc-text-secondary hover:underline"
                  title={`${t('agent.open_config_prefix')}providers.models.${agent.modelProvider}${t('agent.open_config_suffix')}`}
                >
                  {agent.modelProvider}
                </EntityLink>
              ) : (
                <p className="text-xs truncate text-pc-text-muted">
                  {t('agent.no_model_provider')}
                </p>
              )}
            </div>
          </div>
          <button
            ref={closeBtnRef}
            type="button"
            onClick={onClose}
            aria-label={t('agent.close')}
            title={t('agent.close')}
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
              {t('common.status')}
            </span>
            <button
              type="button"
              onClick={() => onToggle(agent)}
              disabled={toggling}
              className="rounded-full transition-opacity disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
              aria-pressed={agent.enabled}
              aria-label={agent.enabled ? t('agent.disable') : t('agent.enable')}
              title={agent.enabled ? t('agent.disable') : t('agent.enable')}
            >
              <Badge tone={agent.enabled ? 'ok' : 'neutral'}>
                <Power className="h-3 w-3" />
                {agent.enabled ? t('agent.enabled') : t('agent.disabled')}
              </Badge>
            </button>
          </div>

          {/* Configuration facts */}
          <DetailGroup icon={Wifi} label={t('agent.section.channels')}>
            {channelCount === 0 ? (
              <span className="text-pc-text-muted">{t('agent.none_bound')}</span>
            ) : (
              agent.channels.map((ch) => (
                <EntityLink
                  key={ch}
                  kind="channel"
                  id={ch}
                  className={CHIP_CLASS}
                  title={`${t('agent.open_config_prefix')}channels.${ch}${t('agent.open_config_suffix')}`}
                >
                  {ch}
                </EntityLink>
              ))
            )}
          </DetailGroup>

          <DetailGroup icon={Shield} label={t('agent.section.profile')}>
            {agent.riskProfile ? (
              <EntityLink
                kind="risk-profile"
                id={agent.riskProfile}
                className="inline-flex items-center gap-1 hover:text-pc-text hover:underline"
                title={t('agent.risk_profile_title')}
              >
                {agent.riskProfile}
              </EntityLink>
            ) : (
              <span
                className="text-pc-text-muted"
                title={t('agent.risk_profile_title')}
              >
                {t('agent.no_risk_profile')}
              </span>
            )}
            <span className="text-pc-text-faint">·</span>
            <EntityLink
              kind="memory-backend"
              id=""
              className="inline-flex items-center gap-1 hover:text-pc-text hover:underline"
              title={
                agent.memoryBackend
                  ? `${t('agent.memory_backend_title_prefix')} ${agent.memoryBackend}`
                  : t('agent.memory_backend_default_title')
              }
            >
              <Database className="h-3 w-3 flex-shrink-0" />
              {agent.memoryBackend || t('agent.memory_backend_default')}
            </EntityLink>
            {agent.runtimeProfile && (
              <>
                <span className="text-pc-text-faint">·</span>
                <EntityLink
                  kind="runtime-profile"
                  id={agent.runtimeProfile}
                  className="inline-flex items-center gap-1 hover:text-pc-text hover:underline"
                  title={t('agent.runtime_profile_title')}
                >
                  <Zap className="h-3 w-3 flex-shrink-0" />
                  {agent.runtimeProfile}
                </EntityLink>
              </>
            )}
          </DetailGroup>

          {skillCount > 0 && (
            <DetailGroup icon={Sparkles} label={t('agent.section.skills')}>
              {agent.skillBundles.map((s) => (
                <EntityLink
                  key={s}
                  kind="skill-bundle"
                  id={s}
                  className={CHIP_CLASS}
                  title={`${t('agent.open_config_prefix')}skill-bundles.${s}${t('agent.open_config_suffix')}`}
                >
                  {s}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {knowledgeCount > 0 && (
            <DetailGroup icon={BookOpen} label={t('agent.section.knowledge')}>
              {agent.knowledgeBundles.map((k) => (
                <EntityLink
                  key={k}
                  kind="knowledge-bundle"
                  id={k}
                  className={CHIP_CLASS}
                  title={`${t('agent.open_config_prefix')}knowledge-bundles.${k}${t('agent.open_config_suffix')}`}
                >
                  {k}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {mcpCount > 0 && (
            <DetailGroup icon={Plug} label={t('agent.section.mcp')}>
              {agent.mcpBundles.map((m) => (
                <EntityLink
                  key={m}
                  kind="mcp-bundle"
                  id={m}
                  className={CHIP_CLASS}
                  title={`${t('agent.open_config_prefix')}mcp-bundles.${m}${t('agent.open_config_suffix')}`}
                >
                  {m}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {peerCount > 0 && (
            <DetailGroup icon={Users} label={t('agent.section.peers')}>
              {agent.peerGroups.map((pg) => (
                <EntityLink
                  key={pg}
                  kind="peer-group"
                  id={pg}
                  className={CHIP_CLASS}
                  title={`${t('agent.open_config_prefix')}peer_groups.${pg}${t('agent.open_config_suffix')}`}
                >
                  {pg}
                </EntityLink>
              ))}
            </DetailGroup>
          )}

          {cronCount > 0 && (
            <DetailGroup icon={Clock} label={t('agent.section.cron')}>
              {agent.cronJobs.map((c) => (
                <EntityLink
                  key={c}
                  kind="cron"
                  id={c}
                  className={CHIP_CLASS}
                  title={`${t('agent.open_config_prefix')}cron.${c}${t('agent.open_config_suffix')}`}
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
                {t('agent.stat.sessions')}
              </div>
              <div className="mt-0.5 text-sm text-pc-text">
                {agent.sessionCount === 0 ? (
                  <span className="text-pc-text-muted">{t('agent.stat.none')}</span>
                ) : (
                  <Link
                    to={`/?tab=sessions&agent=${encodeURIComponent(agent.alias)}`}
                    className="hover:text-pc-accent hover:underline"
                    title={`${t('agent.show_sessions_title')} ${agent.alias}`}
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
                {t('agent.stat.memories')}
              </div>
              <div className="mt-0.5 text-sm text-pc-text">
                {agent.memoryCount === 0 ? (
                  <span className="text-pc-text-muted">{t('agent.stat.none')}</span>
                ) : (
                  <Link
                    to={`/?tab=memories&agent=${encodeURIComponent(agent.alias)}`}
                    className="hover:text-pc-accent hover:underline"
                    title={`${t('agent.show_memories_title')} ${agent.alias}`}
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
                  ? t('agent.cost_untracked_title')
                  : t('agent.cost_tracked_title')
              }
            >
              <div className="flex items-center gap-1 text-[11px] text-pc-text-faint">
                <DollarSign className="h-3 w-3 flex-shrink-0" />
                {t('agent.stat.this_month')}
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
            {t('agent.open_chat')}
          </Link>
          <Link
            to={`/config/agents/${encodeURIComponent(agent.alias)}`}
            className={`${ACTION_BASE} bg-transparent border-pc-border text-pc-text-secondary hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong`}
          >
            <Pencil className="h-4 w-4" />
            {t('agent.edit')}
          </Link>
        </div>
      </div>
    </div>
  );
}
