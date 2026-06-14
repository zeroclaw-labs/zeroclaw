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
  Zap,
} from 'lucide-react';
import type { LucideProps } from 'lucide-react';
import type { AgentSummary } from '@/lib/agents';
import { Card, Badge } from '@/components/ui';
import EntityLink from './EntityLink';

export interface AgentCardProps {
  agent: AgentSummary;
  toggling: boolean;
  onToggle: () => void;
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

// Calm chip: a muted token surface that links into config. Replaces the old
// accent-tinted pill clutter with a single restrained treatment.
const CHIP_CLASS =
  'inline-block font-mono text-[10px] px-2 py-0.5 rounded-full ' +
  'bg-pc-elevated text-pc-text-secondary hover:text-pc-text transition-colors';

// Shared layout for the route-action links. Mirrors the Button primitive's
// shape/size (size="sm") and focus ring so they read as the same component.
const ACTION_BASE =
  'inline-flex items-center justify-center gap-1.5 h-7 px-2.5 text-[13px] ' +
  'font-medium whitespace-nowrap rounded-[var(--radius-md)] border ' +
  'transition-colors duration-150 select-none ' +
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] ' +
  'focus-visible:ring-offset-2 focus-visible:ring-offset-pc-surface';

// A labelled row that groups a set of related facts behind one muted caption +
// icon, so the card reads as a few scannable groups instead of a pill storm.
function FactGroup({
  icon: Icon,
  label,
  children,
}: {
  icon: ComponentType<LucideProps>;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-start gap-2">
      <span className="flex items-center gap-1.5 text-[11px] text-pc-text-faint flex-shrink-0 pt-0.5 w-20">
        <Icon className="h-3 w-3 flex-shrink-0" />
        <span className="truncate">{label}</span>
      </span>
      <div className="flex flex-wrap items-center gap-1 min-w-0 text-xs text-pc-text-secondary">
        {children}
      </div>
    </div>
  );
}

export default function AgentCard({ agent, toggling, onToggle }: AgentCardProps) {
  const channelCount = agent.channels.length;
  const skillCount = agent.skillBundles.length;
  const knowledgeCount = agent.knowledgeBundles.length;
  const mcpCount = agent.mcpBundles.length;
  const cronCount = agent.cronJobs.length;
  const peerCount = agent.peerGroups.length;

  return (
    <Card className="flex flex-col gap-4 p-5">
      {/* Header: identity + enabled state */}
      <div className="flex items-start justify-between gap-3">
        <div className="flex items-center gap-3 min-w-0">
          <div className="h-9 w-9 rounded-[var(--radius-md)] flex-shrink-0 flex items-center justify-center bg-pc-accent/10">
            <Bot className="h-4 w-4 text-pc-accent" />
          </div>
          <div className="min-w-0">
            <EntityLink
              kind="agent"
              id={agent.alias}
              className="block text-[15px] font-semibold truncate text-pc-text hover:underline"
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
          type="button"
          onClick={onToggle}
          disabled={toggling}
          className="flex-shrink-0 rounded-full transition-opacity disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-surface"
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

      {/* Configuration facts, grouped under muted captions */}
      <div className="flex flex-col gap-2">
        <FactGroup icon={Wifi} label="Channels">
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
        </FactGroup>

        <FactGroup icon={Shield} label="Profile">
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
        </FactGroup>

        {skillCount > 0 && (
          <FactGroup icon={Sparkles} label="Skills">
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
          </FactGroup>
        )}

        {knowledgeCount > 0 && (
          <FactGroup icon={BookOpen} label="Knowledge">
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
          </FactGroup>
        )}

        {mcpCount > 0 && (
          <FactGroup icon={Plug} label="MCP">
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
          </FactGroup>
        )}

        {peerCount > 0 && (
          <FactGroup icon={Users} label="Peers">
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
          </FactGroup>
        )}

        {cronCount > 0 && (
          <FactGroup icon={Clock} label="Cron">
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
          </FactGroup>
        )}
      </div>

      {/* Activity stats: sessions / memories / spend */}
      <div className="grid grid-cols-3 gap-2 pt-3 border-t border-pc-border">
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

      {/* Actions. Routes are <Link>s styled to match the Button primitive
          (Button renders a native <button>, so it can't host navigation). */}
      <div className="flex items-center gap-2">
        <Link
          to={`/agent/${encodeURIComponent(agent.alias)}`}
          className={`${ACTION_BASE} flex-1 bg-pc-accent border-transparent text-[#0b1220] hover:bg-pc-accent-light active:brightness-95`}
        >
          <MessageSquare className="h-3.5 w-3.5" />
          Open chat
        </Link>
        <Link
          to={`/config/agents/${encodeURIComponent(agent.alias)}`}
          className={`${ACTION_BASE} bg-transparent border-pc-border text-pc-text-secondary hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong`}
        >
          <Pencil className="h-3.5 w-3.5" />
          Edit
        </Link>
      </div>
    </Card>
  );
}
