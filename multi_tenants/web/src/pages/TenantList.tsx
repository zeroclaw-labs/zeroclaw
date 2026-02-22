import { useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Server, Plus, Loader2, Trash2 } from 'lucide-react';
import { listTenants, createTenant, deleteTenant } from '../api/tenants';
import Layout from '../components/Layout';
import StatusBadge from '../components/StatusBadge';
import Modal from '../components/Modal';
import ConfirmModal from '../components/ConfirmModal';
import FormField from '../components/FormField';
import { useToast } from '../hooks/useToast';

export default function TenantList() {
  const [search, setSearch] = useState('');
  const [showCreate, setShowCreate] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; name: string } | null>(null);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const toast = useToast();

  const { data: tenants = [], isLoading } = useQuery({
    queryKey: ['tenants'],
    queryFn: listTenants,
  });

  const createMut = useMutation({
    mutationFn: createTenant,
    onSuccess: (data) => {
      qc.invalidateQueries({ queryKey: ['tenants'] });
      setShowCreate(false);
      toast.success('Tenant created');
      if (data.status === 'draft') {
        navigate(`/tenants/${data.id}/setup`);
      } else {
        navigate(`/tenants/${data.id}?created=1`);
      }
    },
  });

  const deleteMut = useMutation({
    mutationFn: deleteTenant,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['tenants'] });
      setDeleteTarget(null);
      toast.success('Tenant deleted');
    },
    onError: (err: Error) => {
      setDeleteTarget(null);
      toast.error(err.message || 'Failed to delete');
    },
  });

  const filtered = tenants.filter(t =>
    t.name.toLowerCase().includes(search.toLowerCase()) ||
    t.slug.toLowerCase().includes(search.toLowerCase())
  );

  return (
    <Layout>
      <div className="flex justify-between items-center mb-6">
        <div className="flex items-center gap-3">
          <Server className="h-6 w-6 text-accent-blue" />
          <h1 className="text-2xl font-bold text-text-primary">Tenants</h1>
        </div>
        <button onClick={() => setShowCreate(true)}
          className="px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover transition-colors flex items-center gap-2 font-medium">
          <Plus className="h-4 w-4" />
          Create Tenant
        </button>
      </div>

      <input type="text" placeholder="Search by name or slug..."
        value={search} onChange={e => setSearch(e.target.value)}
        className="w-full mb-4 px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors" />

      {isLoading ? (
        <div className="flex items-center gap-2 text-text-muted">
          <Loader2 className="h-5 w-5 animate-spin" />
          <span>Loading...</span>
        </div>
      ) : (
        <div className="card p-0 overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border-default bg-bg-secondary">
                <th className="text-left px-5 py-3 font-medium text-text-muted">Name</th>
                <th className="text-left px-5 py-3 font-medium text-text-muted">Slug</th>
                <th className="text-left px-5 py-3 font-medium text-text-muted">Status</th>
                <th className="text-left px-5 py-3 font-medium text-text-muted">Plan</th>
                <th className="text-right px-5 py-3 font-medium text-text-muted">Actions</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map(t => (
                <tr key={t.id} className="border-b border-border-subtle last:border-0 hover:bg-bg-card-hover transition-colors">
                  <td className="px-5 py-3">
                    <Link to={t.status === 'draft' ? `/tenants/${t.id}/setup` : `/tenants/${t.id}`}
                      className="text-accent-blue hover:text-accent-blue-hover transition-colors">{t.name}</Link>
                  </td>
                  <td className="px-5 py-3 text-text-muted font-mono">{t.slug}</td>
                  <td className="px-5 py-3"><StatusBadge status={t.status} /></td>
                  <td className="px-5 py-3 text-text-secondary">{t.plan}</td>
                  <td className="px-5 py-3 text-right">
                    {t.status === 'draft' && (
                      <Link to={`/tenants/${t.id}/setup`} className="text-accent-blue hover:text-accent-blue-hover transition-colors text-xs mr-3">Setup</Link>
                    )}
                    <button onClick={() => setDeleteTarget({ id: t.id, name: t.name })}
                      className="text-red-400 hover:text-red-300 transition-colors text-xs inline-flex items-center gap-1">
                      <Trash2 className="h-3 w-3" />
                      Delete
                    </button>
                  </td>
                </tr>
              ))}
              {filtered.length === 0 && (
                <tr><td colSpan={5} className="px-5 py-8 text-center text-text-muted">No tenants found</td></tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      <CreateTenantModal open={showCreate} onClose={() => setShowCreate(false)} onSubmit={createMut.mutate} loading={createMut.isPending} error={createMut.error?.message} />

      <ConfirmModal
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => deleteTarget && deleteMut.mutate(deleteTarget.id)}
        title="Delete Tenant"
        message={`Are you sure you want to delete "${deleteTarget?.name}"? This action cannot be undone.`}
        confirmLabel="Delete"
        danger
        loading={deleteMut.isPending}
      />
    </Layout>
  );
}

function CreateTenantModal({ open, onClose, onSubmit, loading, error }: {
  open: boolean; onClose: () => void;
  onSubmit: (data: Parameters<typeof createTenant>[0]) => void;
  loading: boolean; error?: string;
}) {
  const [name, setName] = useState('');
  const [customSlug, setCustomSlug] = useState('');
  const [plan, setPlan] = useState('free');

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSubmit({
      name, plan,
      ...(customSlug && { custom_slug: customSlug }),
    });
  };

  return (
    <Modal open={open} onClose={onClose} title="Create Tenant">
      <form onSubmit={handleSubmit}>
        <FormField label="Name" value={name} onChange={setName} required placeholder="My Tenant" />
        <FormField label="Slug (optional)" value={customSlug} onChange={setCustomSlug} placeholder="auto-generated if empty" />
        <div className="mb-3">
          <label className="block text-sm font-medium text-text-secondary mb-1">Plan</label>
          <select value={plan} onChange={e => setPlan(e.target.value)}
            className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors">
            <option value="free">Free</option>
            <option value="starter">Starter</option>
            <option value="pro">Pro</option>
            <option value="enterprise">Enterprise</option>
          </select>
        </div>
        <p className="text-xs text-text-muted mb-3">You'll configure the provider, model, and channels in the next step.</p>
        {error && <p className="text-status-error text-sm mb-2">{error}</p>}
        <button type="submit" disabled={loading}
          className="w-full mt-2 px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center justify-center gap-2 font-medium">
          {loading && <Loader2 className="h-4 w-4 animate-spin" />}
          {loading ? 'Creating...' : 'Create & Setup'}
        </button>
      </form>
    </Modal>
  );
}
