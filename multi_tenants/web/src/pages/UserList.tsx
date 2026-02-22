import { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Users, UserPlus, Trash2, Loader2 } from 'lucide-react';
import { listUsers, createUser, deleteUser, updateUser } from '../api/users';
import Layout from '../components/Layout';
import Modal from '../components/Modal';
import FormField from '../components/FormField';

export default function UserList() {
  const [showCreate, setShowCreate] = useState(false);
  const qc = useQueryClient();

  const { data: users = [], isLoading } = useQuery({ queryKey: ['users'], queryFn: listUsers });
  const createMut = useMutation({
    mutationFn: createUser,
    onSuccess: () => { qc.invalidateQueries({ queryKey: ['users'] }); setShowCreate(false); },
  });
  const deleteMut = useMutation({
    mutationFn: deleteUser,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['users'] }),
  });
  const toggleAdminMut = useMutation({
    mutationFn: ({ id, is_super_admin }: { id: string; is_super_admin: boolean }) =>
      updateUser(id, { is_super_admin }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['users'] }),
  });

  return (
    <Layout>
      <div className="flex justify-between items-center mb-6">
        <div className="flex items-center gap-3">
          <Users className="h-6 w-6 text-accent-blue" />
          <h1 className="text-2xl font-bold text-text-primary">Users</h1>
        </div>
        <button onClick={() => setShowCreate(true)}
          className="px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover transition-colors flex items-center gap-2 font-medium">
          <UserPlus className="h-4 w-4" />
          Create User
        </button>
      </div>
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
                <th className="text-left px-5 py-3 font-medium text-text-muted">Email</th>
                <th className="text-left px-5 py-3 font-medium text-text-muted">Name</th>
                <th className="text-left px-5 py-3 font-medium text-text-muted">Admin</th>
                <th className="text-left px-5 py-3 font-medium text-text-muted">Created</th>
                <th className="text-right px-5 py-3 font-medium text-text-muted">Actions</th>
              </tr>
            </thead>
            <tbody>
              {users.map(u => (
                <tr key={u.id} className="border-b border-border-subtle last:border-0 hover:bg-bg-card-hover transition-colors">
                  <td className="px-5 py-3 text-text-primary">{u.email}</td>
                  <td className="px-5 py-3 text-text-secondary">{u.name || '-'}</td>
                  <td className="px-5 py-3">
                    <button
                      onClick={() => toggleAdminMut.mutate({ id: u.id, is_super_admin: !u.is_super_admin })}
                      className={`px-2.5 py-1 text-xs rounded-full font-medium border transition-colors ${
                        u.is_super_admin
                          ? 'bg-green-900/40 text-green-400 border-green-700/50 hover:bg-green-900/60'
                          : 'bg-gray-800 text-gray-400 border-gray-700 hover:bg-gray-700'
                      }`}>
                      {u.is_super_admin ? 'Admin' : 'User'}
                    </button>
                  </td>
                  <td className="px-5 py-3 text-text-muted font-mono">{u.created_at}</td>
                  <td className="px-5 py-3 text-right">
                    {!u.is_super_admin && (
                      <button onClick={() => { if (confirm('Delete this user?')) deleteMut.mutate(u.id); }}
                        className="text-red-400 hover:text-red-300 transition-colors text-xs inline-flex items-center gap-1">
                        <Trash2 className="h-3 w-3" />
                        Delete
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <CreateUserModal open={showCreate} onClose={() => setShowCreate(false)} onSubmit={createMut.mutate} loading={createMut.isPending} />
    </Layout>
  );
}

function CreateUserModal({ open, onClose, onSubmit, loading }: {
  open: boolean; onClose: () => void;
  onSubmit: (data: { email: string; name?: string }) => void;
  loading: boolean;
}) {
  const [email, setEmail] = useState('');
  const [name, setName] = useState('');

  return (
    <Modal open={open} onClose={onClose} title="Create User">
      <form onSubmit={e => { e.preventDefault(); onSubmit({ email, ...(name && { name }) }); }}>
        <FormField label="Email" type="email" value={email} onChange={setEmail} required />
        <FormField label="Name (optional)" value={name} onChange={setName} placeholder="John Doe" />
        <button type="submit" disabled={loading}
          className="w-full mt-2 px-4 py-2 bg-accent-blue text-white rounded-lg text-sm hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center justify-center gap-2 font-medium">
          {loading && <Loader2 className="h-4 w-4 animate-spin" />}
          {loading ? 'Creating...' : 'Create User'}
        </button>
      </form>
    </Modal>
  );
}
