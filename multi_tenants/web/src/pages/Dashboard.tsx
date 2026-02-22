import { useQuery } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { LayoutDashboard, Server, Play, Square, AlertTriangle, Users, MessageSquare, Activity, Cpu, Loader2 } from 'lucide-react';
import { getDashboard, getHealth, getAudit } from '../api/monitoring';
import { listTenants } from '../api/tenants';
import { getAdminResources } from '../api/resources';
import { formatBytes, timeAgo } from '../utils/format';
import Layout from '../components/Layout';
import StatusBadge from '../components/StatusBadge';

const statIcons: Record<string, React.ReactNode> = {
  'Total Tenants': <Server className="h-5 w-5 text-text-muted" />,
  'Running': <Play className="h-5 w-5 text-green-400" />,
  'Stopped': <Square className="h-5 w-5 text-gray-400" />,
  'Error': <AlertTriangle className="h-5 w-5 text-red-400" />,
  'Users': <Users className="h-5 w-5 text-text-muted" />,
  'Channels': <MessageSquare className="h-5 w-5 text-text-muted" />,
};

export default function Dashboard() {
  const { data, isLoading } = useQuery({
    queryKey: ['dashboard'],
    queryFn: getDashboard,
    refetchInterval: 30_000,
  });
  const { data: tenants = [] } = useQuery({
    queryKey: ['tenants'],
    queryFn: listTenants,
    refetchInterval: 30_000,
  });
  const { data: health = [] } = useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
    refetchInterval: 15_000,
  });
  const { data: audit } = useQuery({
    queryKey: ['audit', 'recent'],
    queryFn: () => getAudit(1, 10),
    refetchInterval: 30_000,
  });
  const { data: resources = [] } = useQuery({
    queryKey: ['admin-resources'],
    queryFn: getAdminResources,
    refetchInterval: 30_000,
  });

  return (
    <Layout>
      <div className="flex items-center gap-3 mb-6">
        <LayoutDashboard className="h-6 w-6 text-accent-blue" />
        <h1 className="text-2xl font-bold text-text-primary">Dashboard</h1>
      </div>
      {isLoading ? (
        <div className="flex items-center gap-2 text-text-muted">
          <Loader2 className="h-5 w-5 animate-spin" />
          <span>Loading...</span>
        </div>
      ) : (
        <div className="space-y-6">
          {/* Stats grid */}
          <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-6 gap-4">
            <StatCard label="Total Tenants" value={data?.total_tenants ?? 0} />
            <StatCard label="Running" value={data?.running_tenants ?? 0} valueCls="text-green-400" />
            <StatCard label="Stopped" value={data?.stopped_tenants ?? 0} valueCls="text-gray-400" />
            <StatCard label="Error" value={data?.error_tenants ?? 0} valueCls="text-red-400" />
            <StatCard label="Users" value={data?.total_users ?? 0} />
            <StatCard label="Channels" value={data?.total_channels ?? 0} />
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
            {/* Tenant health */}
            <div className="card p-0">
              <div className="flex justify-between items-center px-5 py-4 border-b border-border-default">
                <div className="flex items-center gap-2">
                  <Activity className="h-4 w-4 text-text-muted" />
                  <h2 className="font-semibold text-text-primary">Tenant Health</h2>
                </div>
                <Link to="/tenants" className="text-xs text-accent-blue hover:text-accent-blue-hover transition-colors">View all</Link>
              </div>
              <div className="divide-y divide-border-subtle">
                {tenants.length === 0 ? (
                  <p className="p-5 text-sm text-text-muted">No tenants</p>
                ) : tenants.slice(0, 8).map(t => {
                  const h = health.find(x => x.tenant_id === t.id);
                  return (
                    <div key={t.id} className="flex items-center justify-between px-5 py-3 hover:bg-bg-card-hover transition-colors">
                      <div className="min-w-0 flex-1">
                        <Link to={t.status === 'draft' ? `/tenants/${t.id}/setup` : `/tenants/${t.id}`}
                          className="text-sm font-medium text-accent-blue hover:text-accent-blue-hover transition-colors truncate block">{t.name}</Link>
                        <span className="text-xs text-text-muted font-mono">{t.slug}</span>
                      </div>
                      <div className="flex items-center gap-3">
                        <StatusBadge status={t.status} />
                        {h?.last_check && (
                          <span className="text-xs text-text-muted font-mono" title={`Last check: ${h.last_check}`}>
                            {new Date(h.last_check).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
                          </span>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>

            {/* Recent activity */}
            <div className="card p-0">
              <div className="flex justify-between items-center px-5 py-4 border-b border-border-default">
                <h2 className="font-semibold text-text-primary">Recent Activity</h2>
                <Link to="/audit" className="text-xs text-accent-blue hover:text-accent-blue-hover transition-colors">View all</Link>
              </div>
              <div className="divide-y divide-border-subtle">
                {!audit?.entries?.length ? (
                  <p className="p-5 text-sm text-text-muted">No recent activity</p>
                ) : audit.entries.slice(0, 8).map(e => (
                  <div key={e.id} className="px-5 py-3 hover:bg-bg-card-hover transition-colors">
                    <div className="flex items-center justify-between">
                      <span className="text-sm font-medium text-text-primary">{e.action}</span>
                      <span className="text-xs text-text-muted font-mono">
                        {new Date(e.created_at).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}
                      </span>
                    </div>
                    <p className="text-xs text-text-secondary truncate">
                      {e.resource}{e.resource_id ? ` (${e.resource_id.slice(0, 8)}...)` : ''}
                      {e.details ? ` — ${e.details}` : ''}
                    </p>
                  </div>
                ))}
              </div>
            </div>
          </div>

          {/* Resource Summary */}
          {resources.length > 0 && (
            <div className="card p-0">
              <div className="flex justify-between items-center px-5 py-4 border-b border-border-default">
                <div className="flex items-center gap-2">
                  <Cpu className="h-4 w-4 text-text-muted" />
                  <h2 className="font-semibold text-text-primary">Resource Usage</h2>
                </div>
              </div>
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-border-default bg-bg-secondary">
                      <th className="text-left px-5 py-3 font-medium text-text-muted">Tenant</th>
                      <th className="text-right px-5 py-3 font-medium text-text-muted">CPU</th>
                      <th className="text-right px-5 py-3 font-medium text-text-muted">Memory</th>
                      <th className="text-right px-5 py-3 font-medium text-text-muted">Disk</th>
                      <th className="text-right px-5 py-3 font-medium text-text-muted">PIDs</th>
                      <th className="text-right px-5 py-3 font-medium text-text-muted">Updated</th>
                    </tr>
                  </thead>
                  <tbody>
                    {[...resources].sort((a, b) => b.cpu_pct - a.cpu_pct).map(r => (
                      <tr key={r.tenant_id} className="border-b border-border-subtle last:border-0 hover:bg-bg-card-hover transition-colors">
                        <td className="px-5 py-3">
                          <Link to={`/tenants/${r.tenant_id}`} className="text-accent-blue hover:text-accent-blue-hover transition-colors">{r.name || r.slug}</Link>
                        </td>
                        <td className={`px-5 py-3 text-right font-mono ${r.cpu_pct > 80 ? 'text-red-400 font-medium' : r.cpu_pct > 50 ? 'text-yellow-400' : 'text-text-primary'}`}>
                          {r.cpu_pct.toFixed(1)}%
                        </td>
                        <td className="px-5 py-3 text-right text-text-primary">
                          {formatBytes(r.mem_bytes)} / {formatBytes(r.mem_limit)}
                        </td>
                        <td className="px-5 py-3 text-right text-text-primary">{formatBytes(r.disk_bytes)}</td>
                        <td className="px-5 py-3 text-right text-text-primary">{r.pids}</td>
                        <td className="px-5 py-3 text-right text-text-muted font-mono">{timeAgo(r.ts)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </div>
      )}
    </Layout>
  );
}

function StatCard({ label, value, valueCls = 'text-text-primary' }: { label: string; value: number; valueCls?: string }) {
  return (
    <div className="card p-4">
      <div className="flex items-center gap-2 mb-2">
        {statIcons[label]}
        <p className="text-sm text-text-muted">{label}</p>
      </div>
      <p className={`text-3xl font-bold ${valueCls}`}>{value}</p>
    </div>
  );
}
