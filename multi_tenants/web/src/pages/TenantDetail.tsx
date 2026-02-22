import { useState } from 'react';
import { useParams, useNavigate, useSearchParams } from 'react-router-dom';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { RotateCw, Square, Trash2, ExternalLink, Activity, Cpu, HardDrive, Gauge, UserPlus, Loader2, X, CheckCircle2, XCircle, AlertTriangle } from 'lucide-react';
import { getTenantResources } from '../api/resources';
import { formatBytes, timeAgo } from '../utils/format';
import {
  listTenants, deleteTenant, restartTenant, stopTenant,
  getTenantLogs, getTenantConfig, updateTenantConfig, testProvider, getPairingCode, resetPairing,
  type Tenant,
} from '../api/tenants';
import { listChannels } from '../api/channels';
import { listMembers, addMember, updateMemberRole, removeMember } from '../api/members';
import { getUsage } from '../api/monitoring';
import Layout from '../components/Layout';
import StatusBadge from '../components/StatusBadge';
import Modal from '../components/Modal';
import FormField from '../components/FormField';
import SetupChecklist from '../components/SetupChecklist';
import CopyButton from '../components/CopyButton';
import ConfirmModal from '../components/ConfirmModal';
import ChannelsTab from './ChannelsTab';
import { useToast } from '../hooks/useToast';
import { PLAN_LIMITS } from '../config/channelSchemas';
import { PROVIDERS, getModels } from '../config/providerSchemas';

const tabs = ['Overview', 'Config', 'Channels', 'Usage', 'Members'] as const;
type Tab = typeof tabs[number];

export default function TenantDetail() {
  const { id } = useParams<{ id: string }>();
  const [tab, setTab] = useState<Tab>('Overview');
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [showStopConfirm, setShowStopConfirm] = useState(false);
  const [showRestartConfirm, setShowRestartConfirm] = useState(false);
  const navigate = useNavigate();
  const qc = useQueryClient();
  const toast = useToast();
  const [searchParams, setSearchParams] = useSearchParams();

  const isNewlyCreated = searchParams.get('created') === '1';
  const [showBanner, setShowBanner] = useState(isNewlyCreated);

  function dismissBanner() {
    setShowBanner(false);
    searchParams.delete('created');
    setSearchParams(searchParams, { replace: true });
  }

  const { data: tenants = [] } = useQuery({
    queryKey: ['tenants'],
    queryFn: listTenants,
    refetchInterval: (query) => {
      const list: Tenant[] = (query.state.data as Tenant[] | undefined) ?? [];
      const t = list.find(x => x.id === id);
      return ['provisioning', 'starting', 'creating'].includes(t?.status ?? '') ? 3_000 : 30_000;
    },
  });

  const tenant = tenants.find(t => t.id === id);

  const deleteMut = useMutation({
    mutationFn: () => deleteTenant(id!),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['tenants'] });
      toast.success('Tenant deleted');
      navigate('/tenants');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to delete tenant'),
  });

  const restartMut = useMutation({
    mutationFn: () => restartTenant(id!),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['tenants'] });
      toast.success('Restart initiated');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to restart'),
  });

  const stopMut = useMutation({
    mutationFn: () => stopTenant(id!),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['tenants'] });
      toast.success('Tenant stopped');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to stop'),
  });

  if (!tenant) return <Layout><p className="text-text-muted">Tenant not found</p></Layout>;

  if (tenant.status === 'draft') {
    navigate(`/tenants/${id}/setup`, { replace: true });
    return null;
  }

  const isTransitional = ['provisioning', 'starting', 'creating'].includes(tenant.status);

  return (
    <Layout>
      {showBanner && (
        <div className="bg-accent-blue/10 border border-accent-blue/30 rounded-xl p-4 mb-6">
          <div className="flex items-start justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm font-semibold text-accent-blue mb-2">
                Tenant "{tenant.name}" created successfully
              </p>
              <div className="flex items-center gap-2 mb-1">
                <span className="text-xs text-text-secondary">Subdomain:</span>
                <span className="text-xs font-mono text-text-primary">{tenant.slug}</span>
                <CopyButton text={tenant.slug} />
              </div>
              <div className="flex items-center gap-2 mb-3">
                <span className="text-xs text-text-secondary">Status:</span>
                <StatusBadge status={tenant.status} />
                {isTransitional && (
                  <span className="text-xs text-accent-blue italic">auto-refreshing while provisioning</span>
                )}
              </div>
              <div className="text-xs text-text-secondary space-y-0.5">
                <p className="font-medium mb-1">Next steps:</p>
                <p>1. Wait for status to become Running (~30s)</p>
                <p>2. Connect a channel (Telegram, Discord, etc.)</p>
                <p>3. Invite team members</p>
              </div>
            </div>
            <button
              type="button"
              onClick={dismissBanner}
              className="flex-shrink-0 text-text-muted hover:text-text-primary transition-colors"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>
      )}

      <div className="flex items-center justify-between mb-4">
        <div>
          <h1 className="text-2xl font-bold text-text-primary">{tenant.name}</h1>
          <p className="text-sm text-text-muted flex items-center gap-2">
            <span className="font-mono">{tenant.slug}</span>
            <span>&middot;</span>
            <StatusBadge status={tenant.status} />
          </p>
        </div>
        <div className="flex gap-2">
          {tenant.status === 'running' && (
            <a
              href={`${window.location.protocol}//${tenant.slug}.${window.location.hostname}/_app/`}
              target="_blank"
              rel="noopener noreferrer"
              className="px-3 py-1.5 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover transition-colors flex items-center gap-1.5"
            >
              <ExternalLink className="h-3.5 w-3.5" />
              Open Dashboard
            </a>
          )}
          <button onClick={() => setShowRestartConfirm(true)}
            className="px-3 py-1.5 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors flex items-center gap-1.5">
            <RotateCw className="h-3.5 w-3.5" />
            Restart
          </button>
          <button onClick={() => setShowStopConfirm(true)}
            className="px-3 py-1.5 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors flex items-center gap-1.5">
            <Square className="h-3.5 w-3.5" />
            Stop
          </button>
          <button
            onClick={() => setShowDeleteConfirm(true)}
            className="px-3 py-1.5 text-sm border border-red-700/50 text-red-400 rounded-lg hover:bg-red-900/20 transition-colors flex items-center gap-1.5"
          >
            <Trash2 className="h-3.5 w-3.5" />
            Delete
          </button>
        </div>
      </div>

      <ConfirmModal
        open={showDeleteConfirm}
        onClose={() => setShowDeleteConfirm(false)}
        onConfirm={() => deleteMut.mutate()}
        title="Delete Tenant"
        message={`Are you sure you want to delete "${tenant.name}"? This will stop the container and remove all data permanently.`}
        confirmLabel="Delete"
        danger
        loading={deleteMut.isPending}
      />
      <ConfirmModal
        open={showStopConfirm}
        onClose={() => setShowStopConfirm(false)}
        onConfirm={() => { stopMut.mutate(); setShowStopConfirm(false); }}
        title="Stop Tenant"
        message={`Stop "${tenant.name}"? The agent will go offline and stop responding to messages.`}
        confirmLabel="Stop"
        loading={stopMut.isPending}
      />
      <ConfirmModal
        open={showRestartConfirm}
        onClose={() => setShowRestartConfirm(false)}
        onConfirm={() => { restartMut.mutate(); setShowRestartConfirm(false); }}
        title="Restart Tenant"
        message={`Restart "${tenant.name}"? The agent will be briefly offline during restart.`}
        confirmLabel="Restart"
        loading={restartMut.isPending}
      />

      <div className="flex gap-1 mb-6 border-b border-border-default">
        {tabs.map(t => (
          <button key={t} onClick={() => setTab(t)}
            className={`px-4 py-2 text-sm font-medium border-b-2 -mb-px transition-colors ${
              tab === t ? 'border-accent-blue text-accent-blue' : 'border-transparent text-text-muted hover:text-text-secondary'
            }`}>
            {t}
          </button>
        ))}
      </div>

      {tab === 'Overview' && <OverviewTab tenant={tenant} onSwitchTab={setTab} />}
      {tab === 'Config' && <ConfigTab tenantId={id!} />}
      {tab === 'Channels' && <ChannelsTab tenantId={id!} plan={tenant.plan} />}
      {tab === 'Usage' && <UsageTab tenantId={id!} />}
      {tab === 'Members' && <MembersTab tenantId={id!} />}
    </Layout>
  );
}

function ResourceCard({ label, value, icon, pct, subtext }: {
  label: string; value: string; icon?: React.ReactNode; pct?: number; subtext?: string
}) {
  return (
    <div className="card p-4">
      <div className="flex items-center gap-2 mb-1">
        {icon}
        <dt className="text-xs text-text-muted">{label}</dt>
      </div>
      <dd className="text-sm font-medium text-text-primary">{value}</dd>
      {pct !== undefined && (
        <div className="w-full bg-gray-800 rounded-full h-1.5 mt-2">
          <div
            className={`h-1.5 rounded-full transition-all ${
              pct > 80 ? 'bg-red-500' : pct > 50 ? 'bg-yellow-500' : 'bg-green-500'
            }`}
            style={{ width: `${Math.min(pct, 100)}%` }}
          />
        </div>
      )}
      {subtext && <dd className="text-xs text-text-muted mt-1">{subtext}</dd>}
    </div>
  );
}

function OverviewTab({ tenant, onSwitchTab }: { tenant: Tenant; onSwitchTab: (tab: Tab) => void }) {
  const qc = useQueryClient();
  const toast = useToast();
  const { data: resources } = useQuery({
    queryKey: ['resources', tenant.id, '1h'],
    queryFn: () => getTenantResources(tenant.id, '1h'),
    refetchInterval: 15_000,
  });
  const { data: pairingData } = useQuery({
    queryKey: ['pairing', tenant.id],
    queryFn: () => getPairingCode(tenant.id),
    enabled: tenant.status === 'running',
  });
  const resetPairingMut = useMutation({
    mutationFn: () => resetPairing(tenant.id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['pairing', tenant.id] });
      qc.invalidateQueries({ queryKey: ['tenants'] });
      toast.success('Pairing reset — new code generated');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to reset pairing'),
  });
  const { data: channels = [] } = useQuery({
    queryKey: ['channels', tenant.id],
    queryFn: () => listChannels(tenant.id),
  });
  const { data: members = [] } = useQuery({
    queryKey: ['members', tenant.id],
    queryFn: () => listMembers(tenant.id),
  });
  const { data: config } = useQuery({
    queryKey: ['config', tenant.id],
    queryFn: () => getTenantConfig(tenant.id),
  });
  const { data: usage = [] } = useQuery({
    queryKey: ['usage', tenant.id, 7],
    queryFn: () => getUsage(tenant.id, 7),
  });
  const { data: logs } = useQuery({
    queryKey: ['logs', tenant.id, 20],
    queryFn: () => getTenantLogs(tenant.id, 20),
    refetchInterval: 15_000,
  });

  const planInfo = PLAN_LIMITS[tenant.plan] ?? PLAN_LIMITS.free;
  const providerDef = config ? PROVIDERS.find(p => p.id === config.provider) : null;
  const todayMessages = usage.length > 0 ? usage[usage.length - 1].messages : 0;
  const weekMessages = usage.reduce((sum, u) => sum + u.messages, 0);

  const dashboardUrl = `${window.location.protocol}//${tenant.slug}.${window.location.hostname}/_app/`;

  const checkItems = [
    {
      label: 'Tenant provisioned',
      done: tenant.status === 'running',
      detail: tenant.status === 'running' ? 'Running' : tenant.status,
    },
    {
      label: 'Agent configured',
      done: !!(config?.provider && config?.api_key_masked !== '****'),
      detail: config ? `${config.provider} / ${config.model}` : undefined,
      action: () => onSwitchTab('Config'),
      actionLabel: 'Configure',
    },
    {
      label: 'Channel connected',
      done: channels.length > 0,
      detail: channels.length > 0 ? `${channels.length} active` : undefined,
      action: () => onSwitchTab('Channels'),
      actionLabel: 'Add Channel',
    },
    {
      label: 'Team members added',
      done: members.length > 0,
      detail: members.length > 0 ? `${members.length} members` : undefined,
      action: () => onSwitchTab('Members'),
      actionLabel: 'Invite',
    },
  ];

  const allDone = checkItems.every(c => c.done);

  return (
    <div className="space-y-6">
      {/* Quick stats */}
      <div className="grid grid-cols-4 gap-4">
        <div className="card p-4">
          <dt className="text-xs text-text-muted mb-1 flex items-center gap-1.5">
            <Activity className="h-3.5 w-3.5" /> Status
          </dt>
          <dd><StatusBadge status={tenant.status} /></dd>
        </div>
        <div className="card p-4">
          <dt className="text-xs text-text-muted mb-1">Provider</dt>
          <dd className="text-sm font-medium text-text-primary truncate">{providerDef?.label ?? config?.provider ?? '—'}</dd>
          {config?.model && <dd className="text-xs text-text-muted truncate">{config.model}</dd>}
        </div>
        <div className="card p-4">
          <dt className="text-xs text-text-muted mb-1">Channels</dt>
          <dd className="text-sm font-medium text-text-primary">{channels.length} / {planInfo.channels}</dd>
          {channels.length > 0 && (
            <dd className="text-xs text-text-muted">{channels.map(c => c.kind).join(', ')}</dd>
          )}
        </div>
        <div className="card p-4">
          <dt className="text-xs text-text-muted mb-1">Messages</dt>
          <dd className="text-sm font-medium text-text-primary">{todayMessages} today</dd>
          <dd className="text-xs text-text-muted">{weekMessages} this week</dd>
        </div>
      </div>

      {/* Resource metrics */}
      {resources?.current ? (
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
          <ResourceCard
            icon={<Cpu className="h-3.5 w-3.5 text-text-muted" />}
            label="CPU" value={`${resources.current.cpu_pct.toFixed(1)}%`} pct={resources.current.cpu_pct} />
          <ResourceCard
            icon={<Gauge className="h-3.5 w-3.5 text-text-muted" />}
            label="Memory"
            value={`${formatBytes(resources.current.mem_bytes)} / ${formatBytes(resources.current.mem_limit)}`}
            pct={resources.current.mem_limit > 0 ? (resources.current.mem_bytes / resources.current.mem_limit) * 100 : 0} />
          <ResourceCard
            icon={<HardDrive className="h-3.5 w-3.5 text-text-muted" />}
            label="Disk" value={formatBytes(resources.current.disk_bytes)} />
          <ResourceCard label="Processes" value={`${resources.current.pids}`} subtext={`Updated ${timeAgo(resources.current.ts)}`} />
        </div>
      ) : tenant.status === 'running' ? (
        <div className="card p-4 text-sm text-text-muted flex items-center gap-2">
          <Loader2 className="h-4 w-4 animate-spin" />
          Loading resource metrics...
        </div>
      ) : null}

      {/* Pairing Code card */}
      {tenant.status === 'running' && pairingData?.pairing_code && (
        <div className="bg-amber-900/20 border border-amber-700/50 rounded-xl p-5">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h3 className="text-sm font-semibold text-amber-200 mb-1">Dashboard Pairing Code</h3>
              <p className="text-xs text-amber-300/80 mb-3">
                Enter this code at{' '}
                <a
                  href={`${window.location.protocol}//${tenant.slug}.${window.location.hostname}/_app/`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="underline font-medium text-amber-200"
                >
                  {tenant.slug}.{window.location.hostname}/_app
                </a>{' '}
                to access the ZeroClaw dashboard. The code can only be used once.
              </p>
              <div className="flex items-center gap-3">
                <span className="font-mono text-3xl font-bold tracking-[0.3em] text-amber-100 bg-amber-900/40 px-4 py-2 rounded-lg border border-amber-700/50">
                  {pairingData.pairing_code}
                </span>
                <CopyButton text={pairingData.pairing_code} />
              </div>
            </div>
            <button
              onClick={() => resetPairingMut.mutate()}
              disabled={resetPairingMut.isPending}
              className="flex-shrink-0 text-xs text-amber-300 hover:text-amber-100 border border-amber-700/50 rounded-lg px-2.5 py-1.5 hover:bg-amber-900/40 disabled:opacity-50 transition-colors"
            >
              {resetPairingMut.isPending ? 'Resetting...' : 'Reset Pairing'}
            </button>
          </div>
        </div>
      )}

      {/* Details card */}
      <div className="card p-6">
        <h2 className="text-lg font-semibold text-text-primary mb-4">Tenant Details</h2>
        <dl className="grid grid-cols-2 gap-4 text-sm">
          <div>
            <dt className="text-text-muted">ID</dt>
            <dd className="font-mono text-xs text-text-secondary">{tenant.id}</dd>
          </div>
          <div>
            <dt className="text-text-muted">Subdomain</dt>
            <dd className="flex items-center gap-2">
              <a
                href={`${window.location.protocol}//${tenant.slug}.${window.location.hostname}/_app/`}
                target="_blank"
                rel="noopener noreferrer"
                className="font-mono text-accent-blue hover:text-accent-blue-hover transition-colors"
              >
                {tenant.slug}.{window.location.hostname}
              </a>
              <CopyButton text={`${tenant.slug}.${window.location.hostname}`} />
            </dd>
          </div>
          <div>
            <dt className="text-text-muted">Plan</dt>
            <dd className="text-text-secondary">
              {tenant.plan} ({planInfo.messages === -1 ? 'unlimited' : planInfo.messages} msg/day,{' '}
              {planInfo.channels} ch, {planInfo.members} members)
            </dd>
          </div>
          <div>
            <dt className="text-text-muted">Port</dt>
            <dd className="text-text-secondary">{tenant.port ?? 'N/A'}</dd>
          </div>
          <div>
            <dt className="text-text-muted">Temperature</dt>
            <dd className="text-text-secondary">{config?.temperature ?? '—'}</dd>
          </div>
          <div>
            <dt className="text-text-muted">Created</dt>
            <dd className="text-text-secondary">{tenant.created_at}</dd>
          </div>
        </dl>
      </div>

      {/* Setup checklist */}
      {!allDone && <SetupChecklist items={checkItems} />}

      {/* Recent logs preview */}
      {logs?.logs && (
        <div className="card p-6">
          <div className="flex justify-between items-center mb-3">
            <h2 className="text-lg font-semibold text-text-primary">Recent Logs</h2>
            <a href={dashboardUrl} target="_blank" rel="noopener noreferrer"
              className="text-xs text-accent-blue hover:text-accent-blue-hover transition-colors">View in Dashboard</a>
          </div>
          <div className="bg-gray-950 text-green-400 p-3 rounded-lg font-mono text-xs overflow-auto max-h-[200px] whitespace-pre-wrap border border-border-subtle">
            {logs.logs.split('\n').slice(-10).join('\n') || 'No logs'}
          </div>
        </div>
      )}
    </div>
  );
}

function UsageTab({ tenantId }: { tenantId: string }) {
  const [days, setDays] = useState(30);
  const { data: usage = [], isLoading } = useQuery({
    queryKey: ['usage', tenantId, days],
    queryFn: () => getUsage(tenantId, days),
  });

  return (
    <div>
      <div className="flex items-center gap-3 mb-4">
        <h2 className="text-lg font-semibold text-text-primary">Usage</h2>
        <select value={days} onChange={e => setDays(Number(e.target.value))}
          className="px-2 py-1 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
          <option value={7}>Last 7 days</option>
          <option value={14}>Last 14 days</option>
          <option value={30}>Last 30 days</option>
          <option value={60}>Last 60 days</option>
          <option value={90}>Last 90 days</option>
        </select>
      </div>
      {isLoading ? (
        <div className="flex items-center gap-2 text-text-muted">
          <Loader2 className="h-5 w-5 animate-spin" />
          <span>Loading...</span>
        </div>
      ) : usage.length === 0 ? (
        <p className="text-text-muted">No usage data for this period</p>
      ) : (
        <>
          {/* Bar chart */}
          <div className="card p-6 mb-4">
            <h3 className="text-sm font-medium text-text-secondary mb-3">Messages per day</h3>
            <div className="flex items-end gap-1 h-32">
              {usage.map(entry => {
                const max = Math.max(...usage.map(u => u.messages), 1);
                const pct = (entry.messages / max) * 100;
                const label = entry.period.slice(0, 13); // "2026-02-21T12"
                return (
                  <div key={entry.period} className="flex-1 flex flex-col items-center group relative">
                    <div className="absolute -top-6 hidden group-hover:block bg-bg-card border border-border-default text-text-primary text-xs rounded-lg px-2 py-1 whitespace-nowrap z-10 shadow-lg">
                      {label}: {entry.messages.toLocaleString()} msgs
                    </div>
                    <div
                      className="w-full bg-accent-blue rounded-t min-h-[2px] transition-all hover:bg-accent-blue-hover"
                      style={{ height: `${Math.max(pct, 2)}%` }}
                    />
                  </div>
                );
              })}
            </div>
            <div className="flex justify-between text-xs text-text-muted mt-1">
              <span>{usage[0]?.period.slice(0, 10)}</span>
              <span>{usage[usage.length - 1]?.period.slice(0, 10)}</span>
            </div>
          </div>

          {/* Summary cards */}
          <div className="grid grid-cols-3 gap-4 mb-4">
            <div className="card p-4">
              <dt className="text-xs text-text-muted">Total Messages</dt>
              <dd className="text-xl font-bold text-text-primary">{usage.reduce((s, u) => s + u.messages, 0).toLocaleString()}</dd>
            </div>
            <div className="card p-4">
              <dt className="text-xs text-text-muted">Tokens In</dt>
              <dd className="text-xl font-bold text-text-primary">{usage.reduce((s, u) => s + u.tokens_in, 0).toLocaleString()}</dd>
            </div>
            <div className="card p-4">
              <dt className="text-xs text-text-muted">Tokens Out</dt>
              <dd className="text-xl font-bold text-text-primary">{usage.reduce((s, u) => s + u.tokens_out, 0).toLocaleString()}</dd>
            </div>
          </div>

          {/* Table */}
          <div className="card p-0 overflow-hidden">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border-default bg-bg-secondary">
                  <th className="text-left px-5 py-3 font-medium text-text-muted">Period</th>
                  <th className="text-right px-5 py-3 font-medium text-text-muted">Messages</th>
                  <th className="text-right px-5 py-3 font-medium text-text-muted">Tokens In</th>
                  <th className="text-right px-5 py-3 font-medium text-text-muted">Tokens Out</th>
                </tr>
              </thead>
              <tbody>
                {usage.map(entry => (
                  <tr key={`${entry.tenant_id}-${entry.period}`} className="border-b border-border-subtle last:border-0 hover:bg-bg-card-hover transition-colors">
                    <td className="px-5 py-3 text-text-secondary font-mono">{entry.period}</td>
                    <td className="px-5 py-3 text-right text-text-primary">{entry.messages.toLocaleString()}</td>
                    <td className="px-5 py-3 text-right text-text-primary">{entry.tokens_in.toLocaleString()}</td>
                    <td className="px-5 py-3 text-right text-text-primary">{entry.tokens_out.toLocaleString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {/* Resource History */}
          <ResourceHistory tenantId={tenantId} />
        </>
      )}
    </div>
  );
}

function MiniBarChart({ data, max: maxOverride, color = 'blue' }: {
  data: number[]; max?: number; color?: string
}) {
  const max = maxOverride ?? Math.max(...data, 1);
  const colorMap: Record<string, string> = {
    blue: 'bg-blue-500 hover:bg-blue-400',
    purple: 'bg-purple-500 hover:bg-purple-400',
    green: 'bg-green-500 hover:bg-green-400',
    orange: 'bg-orange-500 hover:bg-orange-400',
  };
  const barClass = colorMap[color] || colorMap.blue;

  return (
    <div className="flex items-end gap-px h-16">
      {data.map((val, i) => {
        const pct = max > 0 ? (val / max) * 100 : 0;
        return (
          <div key={i} className="flex-1 flex flex-col items-center group relative">
            <div className="absolute -top-5 hidden group-hover:block bg-bg-card border border-border-default text-text-primary text-xs rounded px-1.5 py-0.5 whitespace-nowrap z-10">
              {typeof val === 'number' && val > 1024 ? formatBytes(val) : val.toFixed(1)}
            </div>
            <div
              className={`w-full rounded-t min-h-[1px] transition-all ${barClass}`}
              style={{ height: `${Math.max(pct, 1)}%` }}
            />
          </div>
        );
      })}
    </div>
  );
}

function ResourceHistory({ tenantId }: { tenantId: string }) {
  const [range, setRange] = useState('1h');
  const { data: resources } = useQuery({
    queryKey: ['resources', tenantId, range],
    queryFn: () => getTenantResources(tenantId, range),
    refetchInterval: 30_000,
  });

  if (!resources || resources.history.length === 0) return null;

  const history = resources.history;

  return (
    <div className="mt-6 space-y-4">
      <div className="flex items-center gap-3">
        <h3 className="text-lg font-semibold text-text-primary">Resource History</h3>
        <select value={range} onChange={e => setRange(e.target.value)}
          className="px-2 py-1 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
          <option value="1h">Last 1 hour</option>
          <option value="6h">Last 6 hours</option>
          <option value="24h">Last 24 hours</option>
          <option value="7d">Last 7 days</option>
        </select>
      </div>

      {/* CPU chart */}
      <div className="card p-4">
        <h4 className="text-sm font-medium text-text-secondary mb-2">CPU Usage (%)</h4>
        <MiniBarChart data={history.map(h => h.cpu_pct)} max={100} color="blue" />
      </div>

      {/* Memory chart */}
      <div className="card p-4">
        <h4 className="text-sm font-medium text-text-secondary mb-2">Memory Usage</h4>
        <MiniBarChart
          data={history.map(h => h.mem_limit > 0 ? (h.mem_bytes / h.mem_limit) * 100 : 0)}
          max={100} color="purple" />
      </div>

      {/* Network I/O chart */}
      <div className="card p-4">
        <h4 className="text-sm font-medium text-text-secondary mb-2">Network I/O</h4>
        <div className="grid grid-cols-2 gap-4">
          <div>
            <p className="text-xs text-text-muted mb-1">Inbound</p>
            <MiniBarChart data={history.map(h => h.net_in_bytes)} color="green" />
          </div>
          <div>
            <p className="text-xs text-text-muted mb-1">Outbound</p>
            <MiniBarChart data={history.map(h => h.net_out_bytes)} color="orange" />
          </div>
        </div>
      </div>
    </div>
  );
}

function MembersTab({ tenantId }: { tenantId: string }) {
  const [showAdd, setShowAdd] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<{ id: string; email: string } | null>(null);
  const qc = useQueryClient();
  const toast = useToast();
  const { data: members = [] } = useQuery({
    queryKey: ['members', tenantId],
    queryFn: () => listMembers(tenantId),
  });
  const addMut = useMutation({
    mutationFn: (data: { email: string; role: string }) => addMember(tenantId, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['members', tenantId] });
      setShowAdd(false);
      toast.success('Member invited');
    },
  });
  const roleMut = useMutation({
    mutationFn: ({ mid, role }: { mid: string; role: string }) => updateMemberRole(tenantId, mid, role),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['members', tenantId] });
      toast.success('Role updated');
    },
  });
  const removeMut = useMutation({
    mutationFn: (mid: string) => removeMember(tenantId, mid),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['members', tenantId] });
      setRemoveTarget(null);
      toast.success('Member removed');
    },
  });

  return (
    <div>
      <div className="flex justify-between mb-4">
        <h2 className="text-lg font-semibold text-text-primary">Members</h2>
        <button onClick={() => setShowAdd(true)}
          className="px-3 py-1.5 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover transition-colors flex items-center gap-1.5">
          <UserPlus className="h-3.5 w-3.5" />
          Add Member
        </button>
      </div>
      <div className="card p-0 overflow-hidden">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-border-default bg-bg-secondary">
              <th className="text-left px-5 py-3 font-medium text-text-muted">Email</th>
              <th className="text-left px-5 py-3 font-medium text-text-muted">Role</th>
              <th className="text-left px-5 py-3 font-medium text-text-muted">Joined</th>
              <th className="text-right px-5 py-3 font-medium text-text-muted">Actions</th>
            </tr>
          </thead>
          <tbody>
            {members.map(m => (
              <tr key={m.id} className="border-b border-border-subtle last:border-0 hover:bg-bg-card-hover transition-colors">
                <td className="px-5 py-3 text-text-primary">{m.email}</td>
                <td className="px-5 py-3">
                  <select value={m.role} onChange={e => roleMut.mutate({ mid: m.id, role: e.target.value })}
                    className="px-2 py-1 bg-bg-input border border-border-default rounded-lg text-xs text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
                    <option value="viewer">Viewer</option>
                    <option value="contributor">Contributor</option>
                    <option value="manager">Manager</option>
                    <option value="owner">Owner</option>
                  </select>
                </td>
                <td className="px-5 py-3 text-text-muted font-mono">{m.joined_at}</td>
                <td className="px-5 py-3 text-right">
                  <button onClick={() => setRemoveTarget({ id: m.id, email: m.email })}
                    className="text-red-400 hover:text-red-300 transition-colors text-xs inline-flex items-center gap-1">
                    <Trash2 className="h-3 w-3" />
                    Remove
                  </button>
                </td>
              </tr>
            ))}
            {members.length === 0 && <tr><td colSpan={4} className="px-5 py-8 text-center text-text-muted">No members</td></tr>}
          </tbody>
        </table>
      </div>
      <AddMemberModal open={showAdd} onClose={() => setShowAdd(false)} onSubmit={addMut.mutate} loading={addMut.isPending} />
      <ConfirmModal
        open={!!removeTarget}
        onClose={() => setRemoveTarget(null)}
        onConfirm={() => removeTarget && removeMut.mutate(removeTarget.id)}
        title="Remove Member"
        message={`Remove ${removeTarget?.email} from this tenant?`}
        confirmLabel="Remove"
        danger
        loading={removeMut.isPending}
      />
    </div>
  );
}

function ConfigTab({ tenantId }: { tenantId: string }) {
  const qc = useQueryClient();
  const toast = useToast();

  const { data: config, isLoading } = useQuery({
    queryKey: ['config', tenantId],
    queryFn: () => getTenantConfig(tenantId),
  });

  const [provider, setProvider] = useState('');
  const [model, setModel] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [loaded, setLoaded] = useState(false);
  const [testResult, setTestResult] = useState<{ success: boolean; message: string } | null>(null);

  // Seed form from server config
  if (config && !loaded) {
    setProvider(config.provider || '');
    setModel(config.model || '');
    setApiKey('');
    setLoaded(true);
  }

  const saveMut = useMutation({
    mutationFn: (data: Parameters<typeof updateTenantConfig>[1]) =>
      updateTenantConfig(tenantId, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['config', tenantId] });
      toast.success('Config saved. Agent will restart.');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to save config'),
  });

  const testMut = useMutation({
    mutationFn: () => testProvider(tenantId, {
      provider,
      api_key: apiKey || '____',  // placeholder if unchanged
      model: model || undefined,
    }),
    onSuccess: (res) => setTestResult(res),
    onError: (err: Error) => setTestResult({ success: false, message: err.message }),
  });

  function handleSave(e: React.FormEvent) {
    e.preventDefault();
    const data: Parameters<typeof updateTenantConfig>[1] = {
      provider,
      model,
    };
    if (apiKey.trim()) data.api_key = apiKey.trim();
    saveMut.mutate(data);
  }

  const providerDef = PROVIDERS.find(p => p.id === provider);
  const models = getModels(provider);

  if (isLoading) {
    return (
      <div className="flex items-center gap-2 text-text-muted">
        <Loader2 className="h-5 w-5 animate-spin" />
        <span>Loading config...</span>
      </div>
    );
  }

  return (
    <div>
      <h2 className="text-lg font-semibold text-text-primary mb-4">Agent Configuration</h2>
      <form onSubmit={handleSave} className="space-y-6">
        {/* Provider & Model */}
        <div className="card p-5 space-y-4">
          <h3 className="text-sm font-semibold text-text-secondary uppercase tracking-wider">AI Provider</h3>
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium text-text-secondary mb-1">Provider</label>
              <select
                value={provider}
                onChange={e => { setProvider(e.target.value); setModel(''); setTestResult(null); }}
                className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
              >
                <option value="">Select provider...</option>
                {PROVIDERS.map(p => (
                  <option key={p.id} value={p.id}>{p.label}</option>
                ))}
              </select>
            </div>
            <div>
              <label className="block text-sm font-medium text-text-secondary mb-1">Model</label>
              <select
                value={model}
                onChange={e => setModel(e.target.value)}
                disabled={!provider}
                className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors disabled:opacity-50"
              >
                <option value="">Select model...</option>
                {models.map(m => (
                  <option key={m.id} value={m.id}>
                    {m.label}{m.context ? ` (${m.context})` : ''}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {/* API Key */}
          <div>
            <label className="block text-sm font-medium text-text-secondary mb-1">API Key</label>
            <div className="flex gap-2">
              <input
                type="password"
                value={apiKey}
                onChange={e => { setApiKey(e.target.value); setTestResult(null); }}
                placeholder={config?.api_key_masked ? `Current: ${config.api_key_masked}` : providerDef?.keyPlaceholder || 'Enter API key...'}
                className="flex-1 px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
              />
              <button
                type="button"
                onClick={() => testMut.mutate()}
                disabled={!provider || testMut.isPending}
                className="px-3 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover disabled:opacity-50 transition-colors flex items-center gap-1.5"
              >
                {testMut.isPending ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
                Test
              </button>
            </div>
            {providerDef?.keyHelp && (
              <p className="text-xs text-text-muted mt-1">
                Get key: <a href={providerDef.keyHelp} target="_blank" rel="noopener noreferrer" className="text-accent-blue hover:text-accent-blue-hover">{providerDef.keyHelp}</a>
              </p>
            )}
            {testResult && (
              <div className={`mt-2 flex items-center gap-1.5 text-xs ${testResult.success ? 'text-green-400' : 'text-red-400'}`}>
                {testResult.success ? <CheckCircle2 className="h-3.5 w-3.5" /> : <XCircle className="h-3.5 w-3.5" />}
                {testResult.message}
              </div>
            )}
          </div>
        </div>

        {/* Save */}
        <div className="flex items-center justify-between">
          <p className="text-xs text-amber-400 flex items-center gap-1">
            <AlertTriangle className="h-3.5 w-3.5" />
            Saving will restart the agent container
          </p>
          <button
            type="submit"
            disabled={saveMut.isPending || !provider}
            className="px-4 py-2 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center gap-2"
          >
            {saveMut.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
            {saveMut.isPending ? 'Saving...' : 'Save & Restart'}
          </button>
        </div>
      </form>
    </div>
  );
}

function AddMemberModal({ open, onClose, onSubmit, loading }: {
  open: boolean; onClose: () => void;
  onSubmit: (data: { email: string; role: string }) => void;
  loading: boolean;
}) {
  const [email, setEmail] = useState('');
  const [role, setRole] = useState('viewer');

  return (
    <Modal open={open} onClose={onClose} title="Add Member">
      <form onSubmit={e => { e.preventDefault(); onSubmit({ email, role }); }}>
        <FormField label="Email" type="email" value={email} onChange={setEmail} required />
        <div className="mb-3">
          <label className="block text-sm font-medium text-text-secondary mb-1">Role</label>
          <select value={role} onChange={e => setRole(e.target.value)}
            className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
            <option value="viewer">Viewer</option>
            <option value="contributor">Contributor</option>
            <option value="manager">Manager</option>
            <option value="owner">Owner</option>
          </select>
        </div>
        <button type="submit" disabled={loading}
          className="w-full px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center justify-center gap-2 font-medium">
          {loading && <Loader2 className="h-4 w-4 animate-spin" />}
          {loading ? 'Adding...' : 'Add Member'}
        </button>
      </form>
    </Modal>
  );
}
