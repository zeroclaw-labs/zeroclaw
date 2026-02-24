import { useState, useEffect } from 'react';
import {
  Clock,
  Plus,
  Trash2,
  X,
  CheckCircle,
  XCircle,
  AlertCircle,
} from 'lucide-react';
import type { CronJob } from '@/types/api';
import { getCronJobs, addCronJob, deleteCronJob } from '@/lib/api';

function formatDate(iso: string | null): string {
  if (!iso) return '-';
  const d = new Date(iso);
  return d.toLocaleString();
}

export default function Cron() {
  const [jobs, setJobs] = useState<CronJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  // Form state
  const [formName, setFormName] = useState('');
  const [formSchedule, setFormSchedule] = useState('');
  const [formCommand, setFormCommand] = useState('');
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const fetchJobs = () => {
    setLoading(true);
    getCronJobs()
      .then(setJobs)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    fetchJobs();
  }, []);

  const handleAdd = async () => {
    if (!formSchedule.trim() || !formCommand.trim()) {
      setFormError('Schedule and command are required.');
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      const job = await addCronJob({
        name: formName.trim() || undefined,
        schedule: formSchedule.trim(),
        command: formCommand.trim(),
      });
      setJobs((prev) => [...prev, job]);
      setShowForm(false);
      setFormName('');
      setFormSchedule('');
      setFormCommand('');
    } catch (err: unknown) {
      setFormError(err instanceof Error ? err.message : 'Failed to add job');
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await deleteCronJob(id);
      setJobs((prev) => prev.filter((j) => j.id !== id));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to delete job');
    } finally {
      setConfirmDelete(null);
    }
  };

  const statusIcon = (status: string | null) => {
    if (!status) return null;
    switch (status.toLowerCase()) {
      case 'ok':
      case 'success':
        return <CheckCircle className="h-4 w-4 text-green-400" />;
      case 'error':
      case 'failed':
        return <XCircle className="h-4 w-4 text-red-400" />;
      default:
        return <AlertCircle className="h-4 w-4 text-yellow-400" />;
    }
  };

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load cron jobs: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Clock className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">
            Scheduled Tasks ({jobs.length})
          </h2>
        </div>
        <button
          onClick={() => setShowForm(true)}
          className="flex items-center gap-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium px-4 py-2 rounded-lg transition-colors"
        >
          <Plus className="h-4 w-4" />
          Add Job
        </button>
      </div>

      {/* Add Job Form Modal */}
      {showForm && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
          <div className="bg-gray-900 border border-gray-700 rounded-xl p-6 w-full max-w-md mx-4">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold text-white">Add Cron Job</h3>
              <button
                onClick={() => {
                  setShowForm(false);
                  setFormError(null);
                }}
                className="text-gray-400 hover:text-white transition-colors"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {formError && (
              <div className="mb-4 rounded-lg bg-red-900/30 border border-red-700 p-3 text-sm text-red-300">
                {formError}
              </div>
            )}

            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-1">
                  Name (optional)
                </label>
                <input
                  type="text"
                  value={formName}
                  onChange={(e) => setFormName(e.target.value)}
                  placeholder="e.g. Daily cleanup"
                  className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-1">
                  Schedule <span className="text-red-400">*</span>
                </label>
                <input
                  type="text"
                  value={formSchedule}
                  onChange={(e) => setFormSchedule(e.target.value)}
                  placeholder="e.g. 0 0 * * * (cron expression)"
                  className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-gray-300 mb-1">
                  Command <span className="text-red-400">*</span>
                </label>
                <input
                  type="text"
                  value={formCommand}
                  onChange={(e) => setFormCommand(e.target.value)}
                  placeholder="e.g. cleanup --older-than 7d"
                  className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
              </div>
            </div>

            <div className="flex justify-end gap-3 mt-6">
              <button
                onClick={() => {
                  setShowForm(false);
                  setFormError(null);
                }}
                className="px-4 py-2 text-sm font-medium text-gray-300 hover:text-white border border-gray-700 rounded-lg hover:bg-gray-800 transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={handleAdd}
                disabled={submitting}
                className="px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 rounded-lg transition-colors disabled:opacity-50"
              >
                {submitting ? 'Adding...' : 'Add Job'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Jobs Table */}
      {jobs.length === 0 ? (
        <div className="bg-gray-900 rounded-xl border border-gray-800 p-8 text-center">
          <Clock className="h-10 w-10 text-gray-600 mx-auto mb-3" />
          <p className="text-gray-400">No scheduled tasks configured.</p>
        </div>
      ) : (
        <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  ID
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Name
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Command
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Next Run
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Last Status
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Enabled
                </th>
                <th className="text-right px-4 py-3 text-gray-400 font-medium">
                  Actions
                </th>
              </tr>
            </thead>
            <tbody>
              {jobs.map((job) => (
                <tr
                  key={job.id}
                  className="border-b border-gray-800/50 hover:bg-gray-800/30 transition-colors"
                >
                  <td className="px-4 py-3 text-gray-400 font-mono text-xs">
                    {job.id.slice(0, 8)}
                  </td>
                  <td className="px-4 py-3 text-white font-medium">
                    {job.name ?? '-'}
                  </td>
                  <td className="px-4 py-3 text-gray-300 font-mono text-xs max-w-[200px] truncate">
                    {job.command}
                  </td>
                  <td className="px-4 py-3 text-gray-400 text-xs">
                    {formatDate(job.next_run)}
                  </td>
                  <td className="px-4 py-3">
                    <div className="flex items-center gap-1.5">
                      {statusIcon(job.last_status)}
                      <span className="text-gray-300 text-xs capitalize">
                        {job.last_status ?? '-'}
                      </span>
                    </div>
                  </td>
                  <td className="px-4 py-3">
                    <span
                      className={`inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium ${
                        job.enabled
                          ? 'bg-green-900/40 text-green-400 border border-green-700/50'
                          : 'bg-gray-800 text-gray-500 border border-gray-700'
                      }`}
                    >
                      {job.enabled ? 'Enabled' : 'Disabled'}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-right">
                    {confirmDelete === job.id ? (
                      <div className="flex items-center justify-end gap-2">
                        <span className="text-xs text-red-400">Delete?</span>
                        <button
                          onClick={() => handleDelete(job.id)}
                          className="text-red-400 hover:text-red-300 text-xs font-medium"
                        >
                          Yes
                        </button>
                        <button
                          onClick={() => setConfirmDelete(null)}
                          className="text-gray-400 hover:text-white text-xs font-medium"
                        >
                          No
                        </button>
                      </div>
                    ) : (
                      <button
                        onClick={() => setConfirmDelete(job.id)}
                        className="text-gray-400 hover:text-red-400 transition-colors"
                      >
                        <Trash2 className="h-4 w-4" />
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
