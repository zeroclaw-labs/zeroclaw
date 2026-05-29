import type { ReactNode, ComponentType } from 'react';
import { Link } from 'react-router-dom';
import {
  BookOpen,
  Bot,
  Brain,
  Clock,
  Database,
  DollarSign,
  ExternalLink,
  MessageSquare,
  Pencil,
  Plug,
  Power,
  Server,
  Shield,
  Sparkles,
  Users,
  Wifi,
  Zap,
} from 'lucide-react';
import type { LucideProps } from 'lucide-react';
import type { AgentSummary } from '@/lib/agents';
import EntityLink from './EntityLink';

function ChipRow({
  icon: Icon,
  children,
}: {
  icon: ComponentType<LucideProps>;
  children: ReactNode;
}) {
  return (
    <div
      className="flex items-start gap-1.5 flex-wrap text-xs"
      style={{ color: 'var(--pc-text-muted)' }}
    >
      <Icon className="h-3 w-3 mt-0.5 flex-shrink-0" />
      <div className="flex flex-wrap gap-1 min-w-0">{children}</div>
    </div>
  );
}

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

const CHIP_CLASS =
  'font-mono text-[10px] px-1.5 py-0.5 rounded-full hover:underline';
const CHIP_STYLE = {
  background: 'rgba(var(--pc-accent-rgb), 0.08)',
  color: 'var(--pc-text-secondary)',
};

export default function AgentCard({ agent, toggling, onToggle }: AgentCardProps) {
  const channelCount = agent.channels.length;
  const skillCount = agent.skillBundles.length;
  const knowledgeCount = agent.knowledgeBundles.length;
  const mcpCount = agent.mcpBundles.length;
  const cronCount = agent.cronJobs.length;
  const peerCount = agent.peerGroups.length;
  return (
    <div
      className="rounded-2xl border p-5 transition-colors"
      style={{
        background: 'var(--pc-bg-surface)',
        borderColor: 'var(--pc-border)',
      }}
    >
      <div className="flex items-start justify-between mb-3">
        <div className="flex items-center gap-2 min-w-0">
          <div
            className="h-9 w-9 rounded-xl flex-shrink-0 flex items-center justify-center"
            style={{ background: 'var(--pc-accent-glow)' }}
          >
            <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
          </div>
          <div className="min-w-0">
            <EntityLink
              kind="agent"
              id={agent.alias}
              className="text-sm font-semibold truncate hover:underline"
              title={`Open agents.${agent.alias} config`}
            >
              <span style={{ color: 'var(--pc-text-primary)' }}>{agent.alias}</span>
            </EntityLink>
            {agent.modelProvider ? (
              <EntityLink
                kind="model-provider"
                id={agent.modelProvider}
                className="text-xs truncate font-mono block hover:underline"
                title={`Open providers.models.${agent.modelProvider} config`}
              >
                <span style={{ color: 'var(--pc-text-muted)' }}>
                  {agent.modelProvider}
                </span>
              </EntityLink>
            ) : (
              <p className="text-xs truncate" style={{ color: 'var(--pc-text-muted)' }}>
                no model_provider set
              </p>
            )}
          </div>
        </div>
        <button
          type="button"
          onClick={onToggle}
          disabled={toggling}
          className="flex items-center gap-1 px-2 py-1 rounded-lg text-[10px] font-medium transition-colors disabled:opacity-50"
          style={{
            background: agent.enabled
              ? 'var(--color-status-success-alpha-08)'
              : 'var(--pc-bg-elevated)',
            color: agent.enabled
              ? 'var(--color-status-success)'
              : 'var(--pc-text-muted)',
            border: '1px solid',
            borderColor: agent.enabled
              ? 'var(--color-status-success-alpha-20)'
              : 'var(--pc-border)',
          }}
          aria-pressed={agent.enabled}
          aria-label={agent.enabled ? 'Disable agent' : 'Enable agent'}
        >
          <Power className="h-3 w-3" />
          {agent.enabled ? 'enabled' : 'disabled'}
        </button>
      </div>

      <div className="flex flex-col gap-1.5 mb-4">
        {/* Gateway info — dedicated per-agent gateway (PR #10) gets an
            "isolated" badge + click-through to its URL; shared agents
            get a compact "shared :port" chip. Hidden when no gateway
            info has been resolved (older daemons without /api/agents/summary). */}
        {agent.gatewayPort !== null && (
          <ChipRow icon={Server}>
            {agent.dedicatedGateway && agent.gatewayUrl ? (
              <a
                href={agent.gatewayUrl}
                target="_blank"
                rel="noreferrer noopener"
                className={`${CHIP_CLASS} inline-flex items-center gap-1`}
                style={{
                  ...CHIP_STYLE,
                  color: 'var(--color-status-success)',
                  background: 'var(--color-status-success-alpha-08)',
                }}
                title={`Dedicated per-agent gateway listening on ${agent.gatewayUrl}`}
              >
                <span>isolated · :{agent.gatewayPort}</span>
                <ExternalLink className="h-2.5 w-2.5" />
              </a>
            ) : (
              <span
                className={CHIP_CLASS}
                style={CHIP_STYLE}
                title="Sharing the global gateway port"
              >
                shared · :{agent.gatewayPort}
              </span>
            )}
          </ChipRow>
        )}

        {channelCount === 0 ? (
          <ChipRow icon={Wifi}>
            <span>No channels bound</span>
          </ChipRow>
        ) : (
          <ChipRow icon={Wifi}>
            {agent.channels.map((ch) => (
              <EntityLink
                key={ch}
                kind="channel"
                id={ch}
                className={CHIP_CLASS}
                title={`Open channels.${ch} config`}
              >
                <span style={CHIP_STYLE} className="inline-block px-1.5 py-0.5 rounded-full">
                  {ch}
                </span>
              </EntityLink>
            ))}
          </ChipRow>
        )}

        <div
          className="flex items-center gap-3 text-xs flex-wrap"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          {agent.riskProfile ? (
            <EntityLink
              kind="risk-profile"
              id={agent.riskProfile}
              className="inline-flex items-center gap-1 hover:underline"
              title="Risk profile (autonomy/sandbox tier)"
            >
              <Shield className="h-3 w-3" />
              {agent.riskProfile}
            </EntityLink>
          ) : (
            <span className="inline-flex items-center gap-1" title="Risk profile (autonomy/sandbox tier)">
              <Shield className="h-3 w-3" />
              no risk profile
            </span>
          )}
          <EntityLink
            kind="memory-backend"
            id=""
            className="inline-flex items-center gap-1 hover:underline"
            title={
              agent.memoryBackend
                ? `Memory backend: ${agent.memoryBackend}`
                : 'No per-agent override. Inherits the default backend (sqlite) from [memory].'
            }
          >
            <Database className="h-3 w-3" />
            {agent.memoryBackend || 'sqlite (default)'}
          </EntityLink>
          {agent.runtimeProfile && (
            <EntityLink
              kind="runtime-profile"
              id={agent.runtimeProfile}
              className="inline-flex items-center gap-1 hover:underline"
              title="Runtime profile (loop, token limits, retries)"
            >
              <Zap className="h-3 w-3" />
              {agent.runtimeProfile}
            </EntityLink>
          )}
        </div>

        {skillCount > 0 && (
          <ChipRow icon={Sparkles}>
            {agent.skillBundles.map((s) => (
              <EntityLink
                key={s}
                kind="skill-bundle"
                id={s}
                className={CHIP_CLASS}
                title={`Open skill-bundles.${s} config`}
              >
                <span style={CHIP_STYLE} className="inline-block px-1.5 py-0.5 rounded-full">
                  {s}
                </span>
              </EntityLink>
            ))}
          </ChipRow>
        )}

        {knowledgeCount > 0 && (
          <ChipRow icon={BookOpen}>
            {agent.knowledgeBundles.map((k) => (
              <EntityLink
                key={k}
                kind="knowledge-bundle"
                id={k}
                className={CHIP_CLASS}
                title={`Open knowledge-bundles.${k} config`}
              >
                <span style={CHIP_STYLE} className="inline-block px-1.5 py-0.5 rounded-full">
                  {k}
                </span>
              </EntityLink>
            ))}
          </ChipRow>
        )}

        {mcpCount > 0 && (
          <ChipRow icon={Plug}>
            {agent.mcpBundles.map((m) => (
              <EntityLink
                key={m}
                kind="mcp-bundle"
                id={m}
                className={CHIP_CLASS}
                title={`Open mcp-bundles.${m} config`}
              >
                <span style={CHIP_STYLE} className="inline-block px-1.5 py-0.5 rounded-full">
                  {m}
                </span>
              </EntityLink>
            ))}
          </ChipRow>
        )}

        {peerCount > 0 && (
          <ChipRow icon={Users}>
            {agent.peerGroups.map((pg) => (
              <EntityLink
                key={pg}
                kind="peer-group"
                id={pg}
                className={CHIP_CLASS}
                title={`Open peer-groups.${pg} config`}
              >
                <span style={CHIP_STYLE} className="inline-block px-1.5 py-0.5 rounded-full">
                  {pg}
                </span>
              </EntityLink>
            ))}
          </ChipRow>
        )}

        {cronCount > 0 && (
          <ChipRow icon={Clock}>
            {agent.cronJobs.map((c) => (
              <EntityLink
                key={c}
                kind="cron"
                id={c}
                className={CHIP_CLASS}
                title={`Open cron.${c} config`}
              >
                <span style={CHIP_STYLE} className="inline-block px-1.5 py-0.5 rounded-full">
                  {c}
                </span>
              </EntityLink>
            ))}
          </ChipRow>
        )}

        <p
          className="text-xs flex items-center gap-1.5"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          <MessageSquare className="h-3 w-3" />
          {agent.sessionCount === 0 ? (
            <span>No sessions</span>
          ) : (
            <Link
              to={`/?tab=sessions&agent=${encodeURIComponent(agent.alias)}`}
              className="hover:underline"
              title={`Show sessions for ${agent.alias}`}
            >
              {agent.sessionCount === 1
                ? '1 session'
                : `${agent.sessionCount} sessions`}
            </Link>
          )}
          <span
            className="inline-flex items-center gap-1 ml-2"
            style={{ color: 'var(--pc-text-faint)' }}
          >
            <Clock className="h-3 w-3" />
            {formatRelative(agent.lastActivity)}
          </span>
        </p>
        <p
          className="text-xs flex items-center gap-1.5"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          <Brain className="h-3 w-3" />
          {agent.memoryCount === 0 ? (
            <span>No memories</span>
          ) : (
            <Link
              to={`/?tab=memories&agent=${encodeURIComponent(agent.alias)}`}
              className="hover:underline"
              title={`Show memories for ${agent.alias}`}
            >
              {agent.memoryCount === 1
                ? '1 memory'
                : `${agent.memoryCount} memories`}
            </Link>
          )}
        </p>
        <p
          className="text-xs flex items-center gap-1.5"
          style={{ color: 'var(--pc-text-muted)' }}
          title={
            agent.monthCostUsd === null
              ? 'Per-agent tracking disabled in [cost].track_per_agent'
              : 'Month-to-date spend attributed to this agent'
          }
        >
          <DollarSign className="h-3 w-3" />
          {formatUsd(agent.monthCostUsd)} this month
        </p>
      </div>

      <div className="flex items-center gap-2">
        <Link
          to={`/agent/${encodeURIComponent(agent.alias)}`}
          className="btn-electric flex-1 flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-xl text-xs"
        >
          <MessageSquare className="h-3.5 w-3.5" />
          Open chat
        </Link>
        <Link
          to={`/config/agents/${encodeURIComponent(agent.alias)}`}
          className="btn-secondary flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-xl text-xs"
        >
          <Pencil className="h-3.5 w-3.5" />
          Edit
        </Link>
      </div>
    </div>
  );
}
