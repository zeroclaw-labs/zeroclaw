import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { ScrollText, Loader2, X } from 'lucide-react';
import { getAudit } from '../api/monitoring';
import Layout from '../components/Layout';
import Pagination from '../components/Pagination';

export default function AuditLog() {
  const [page, setPage] = useState(1);
  const perPage = 50;
  const [actionFilter, setActionFilter] = useState('');
  const [actorFilter, setActorFilter] = useState('');
  const [sinceFilter, setSinceFilter] = useState('');

  const { data, isLoading } = useQuery({
    queryKey: ['audit', page, actionFilter, actorFilter, sinceFilter],
    queryFn: () => getAudit(page, perPage, {
      action: actionFilter || undefined,
      actor_id: actorFilter || undefined,
      since: sinceFilter || undefined,
    }),
  });

  const hasFilters = actionFilter || actorFilter || sinceFilter;

  return (
    <Layout>
      <div className="flex items-center gap-3 mb-6">
        <ScrollText className="h-6 w-6 text-accent-blue" />
        <h1 className="text-2xl font-bold text-text-primary">Audit Log</h1>
      </div>
      <div className="flex gap-3 mb-4 flex-wrap items-center">
        <select value={actionFilter} onChange={e => { setActionFilter(e.target.value); setPage(1); }}
          className="px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
          <option value="">All actions</option>
          <option value="login_success">Login</option>
          <option value="logout">Logout</option>
          <option value="tenant_created">Tenant Created</option>
          <option value="tenant_deleted">Tenant Deleted</option>
          <option value="tenant_config_updated">Config Updated</option>
          <option value="member_added">Member Added</option>
          <option value="member_role_updated">Role Updated</option>
          <option value="member_removed">Member Removed</option>
          <option value="user_created">User Created</option>
          <option value="user_deleted">User Deleted</option>
          <option value="user_updated">User Updated</option>
        </select>
        <input type="text" value={actorFilter} onChange={e => { setActorFilter(e.target.value); setPage(1); }}
          placeholder="Actor ID..."
          className="px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted w-48 focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors" />
        <input type="date" value={sinceFilter} onChange={e => { setSinceFilter(e.target.value); setPage(1); }}
          className="px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors" />
        {hasFilters && (
          <button onClick={() => { setActionFilter(''); setActorFilter(''); setSinceFilter(''); setPage(1); }}
            className="px-3 py-2 text-sm text-accent-blue hover:text-accent-blue-hover transition-colors inline-flex items-center gap-1">
            <X className="h-3.5 w-3.5" />
            Clear filters
          </button>
        )}
      </div>
      {isLoading ? (
        <div className="flex items-center gap-2 text-text-muted">
          <Loader2 className="h-5 w-5 animate-spin" />
          <span>Loading...</span>
        </div>
      ) : (
        <>
          <div className="card p-0 overflow-hidden">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border-default bg-bg-secondary">
                  <th className="text-left px-5 py-3 font-medium text-text-muted">Time</th>
                  <th className="text-left px-5 py-3 font-medium text-text-muted">Action</th>
                  <th className="text-left px-5 py-3 font-medium text-text-muted">Resource</th>
                  <th className="text-left px-5 py-3 font-medium text-text-muted">Actor</th>
                  <th className="text-left px-5 py-3 font-medium text-text-muted">Details</th>
                </tr>
              </thead>
              <tbody>
                {data?.entries.map(entry => (
                  <tr key={entry.id} className="border-b border-border-subtle last:border-0 hover:bg-bg-card-hover transition-colors">
                    <td className="px-5 py-3 text-text-muted font-mono whitespace-nowrap">{entry.created_at}</td>
                    <td className="px-5 py-3 font-medium text-text-primary">{entry.action}</td>
                    <td className="px-5 py-3 text-text-secondary">{entry.resource}{entry.resource_id ? ` (${entry.resource_id.slice(0, 8)}...)` : ''}</td>
                    <td className="px-5 py-3 text-text-muted font-mono">{entry.actor_id?.slice(0, 8) || 'system'}...</td>
                    <td className="px-5 py-3 text-text-secondary truncate max-w-xs">{entry.details || '-'}</td>
                  </tr>
                ))}
                {(!data?.entries || data.entries.length === 0) && (
                  <tr><td colSpan={5} className="px-5 py-8 text-center text-text-muted">No audit entries</td></tr>
                )}
              </tbody>
            </table>
          </div>
          {data && <Pagination page={page} total={data.total} perPage={perPage} onChange={setPage} />}
        </>
      )}
    </Layout>
  );
}
