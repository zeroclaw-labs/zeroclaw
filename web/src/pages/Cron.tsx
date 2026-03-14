import { Fragment, useEffect, useState } from 'react';
import {
  Clock,
  Plus,
  Trash2,
  X,
  CheckCircle,
  XCircle,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  RefreshCw,
} from 'lucide-react';
import type { CronJob, CronRun } from '@/types/api';
import { getCronJobs, addCronJob, deleteCronJob, getCronRuns } from '@/lib/api';

function formatDate(iso: string | null): string {
  if (!iso) return '-';
  const d = new Date(iso);
  return d.toLocaleString();
}

export default function Cron() {
  const [jobs, setJobs] = useState<CronJob[]>([]);
  const [runsByJob, setRunsByJob] = useState<Record<string, CronRun[]>>({});
  const [runsLoadingByJob, setRunsLoadingByJob] = useState<Record<string, boolean>>({});
  const [runsErrorByJob, setRunsErrorByJob] = useState<Record<string, string | null>>({});
  const [expandedJob, setExpandedJob] = useState<string | null>(null);
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

  const fetchRuns = async (jobId: string, force = false) => {
    if (runsLoadingByJob[jobId]) return;
    if (!force && runsByJob[jobId]) return;

    setRunsLoadingByJob((prev) => ({ ...prev, [jobId]: true }));
    setRunsErrorByJob((prev) => ({ ...prev, [jobId]: null }));

    try {
      const runs = await getCronRuns(jobId, 10);
      setRunsByJob((prev) => ({ ...prev, [jobId]: runs }));
    } catch (err: unknown) {
      setRunsErrorByJob((prev) => ({
        ...prev,
        [jobId]: err instanceof Error ? err.message : 'Failed to load run history',
      }));
    } finally {
      setRunsLoadingByJob((prev) => ({ ...prev, [jobId]: false }));
    }
  };

  const handleAdd = async () => {
    if (!formSchedule.trim() || !formCommand.trim()) {
      setFormError('Schedule and command are required.');
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      await addCronJob({
        name: formName.trim() || undefined,
        schedule: formSchedule.trim(),
        command: formCommand.trim(),
      });
      fetchJobs();
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

  const toggleRuns = async (jobId: string) => {
    const nextExpanded = expandedJob === jobId ? null : jobId;
    setExpandedJob(nextExpanded);
    if (nextExpanded) {
      await fetchRuns(jobId);
    }
  };

  const formatDuration = (durationMs: number | null): string => {
    if (durationMs === null) return '-';
    if (durationMs < 1000) return `${durationMs} ms`;
    return `${(durationMs / 1000).toFixed(durationMs < 10_000 ? 1 : 0)} s`;
  };

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-4 text-[#ff6680]">
          Failed to load cron jobs: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Clock className="h-5 w-5 text-[#0080ff]" />
          <h2 className="text-sm font-semibold text-white uppercase tracking-wider">
            Scheduled Tasks ({jobs.length})
          </h2>
        </div>
        <button
          onClick={() => setShowForm(true)}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          <Plus className="h-4 w-4" />
          Add Job
        </button>
      </div>

      {/* Add Job Form Modal */}
      {showForm && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="glass-card p-6 w-full max-w-md mx-4 animate-fade-in-scale">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold text-white">Add Cron Job</h3>
              <button
                onClick={() => {
                  setShowForm(false);
                  setFormError(null);
                }}
                className="text-[#556080] hover:text-white transition-colors duration-300"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {formError && (
              <div className="mb-4 rounded-xl bg-[#ff446615] border border-[#ff446630] p-3 text-sm text-[#ff6680] animate-fade-in">
                {formError}
              </div>
            )}

            <div className="space-y-4">
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  Name (optional)
                </label>
                <input
                  type="text"
                  value={formName}
                  onChange={(e) => setFormName(e.target.value)}
                  placeholder="e.g. Daily cleanup"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  Schedule <span className="text-[#ff4466]">*</span>
                </label>
                <input
                  type="text"
                  value={formSchedule}
                  onChange={(e) => setFormSchedule(e.target.value)}
                  placeholder="e.g. 0 0 * * * (cron expression)"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  Command <span className="text-[#ff4466]">*</span>
                </label>
                <input
                  type="text"
                  value={formCommand}
                  onChange={(e) => setFormCommand(e.target.value)}
                  placeholder="e.g. cleanup --older-than 7d"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
            </div>

            <div className="flex justify-end gap-3 mt-6">
              <button
                onClick={() => {
                  setShowForm(false);
                  setFormError(null);
                }}
                className="px-4 py-2 text-sm font-medium text-[#8892a8] hover:text-white border border-[#1a1a3e] rounded-xl hover:bg-[#0080ff08] transition-all duration-300"
              >
                Cancel
              </button>
              <button
                onClick={handleAdd}
                disabled={submitting}
                className="btn-electric px-4 py-2 text-sm font-medium"
              >
                {submitting ? 'Adding...' : 'Add Job'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Jobs Table */}
      {jobs.length === 0 ? (
        <div className="glass-card p-8 text-center">
          <Clock className="h-10 w-10 text-[#1a1a3e] mx-auto mb-3" />
          <p className="text-[#556080]">No scheduled tasks configured.</p>
        </div>
      ) : (
        <div className="glass-card overflow-x-auto">
          <table className="table-electric">
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
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Runs
                </th>
                <th className="text-right px-4 py-3 text-gray-400 font-medium">
                  Actions
                </th>
              </tr>
            </thead>
            <tbody>
              {jobs.map((job) => {
                const isExpanded = expandedJob === job.id;
                const runs = runsByJob[job.id] ?? [];
                const runsLoading = runsLoadingByJob[job.id];
                const runsError = runsErrorByJob[job.id];

                return (
                  <Fragment key={job.id}>
                    <tr className="border-b border-gray-800/50 hover:bg-gray-800/30 transition-colors">
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
                      <td className="px-4 py-3">
                        <button
                          onClick={() => void toggleRuns(job.id)}
                          className="inline-flex items-center gap-1.5 text-sm text-blue-300 hover:text-blue-200 transition-colors"
                        >
                          {isExpanded ? (
                            <ChevronDown className="h-4 w-4" />
                          ) : (
                            <ChevronRight className="h-4 w-4" />
                          )}
                          History
                        </button>
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
                    {isExpanded && (
                      <tr className="border-b border-gray-800/50 bg-gray-950/50">
                        <td colSpan={8} className="px-4 py-4">
                          <div className="mb-3 flex items-center justify-between">
                            <div>
                              <h3 className="text-sm font-semibold text-white">Recent Runs</h3>
                              <p className="text-xs text-gray-500">
                                Last 10 recorded executions for this job
                              </p>
                            </div>
                            <button
                              onClick={() => void fetchRuns(job.id, true)}
                              className="inline-flex items-center gap-1.5 rounded-lg border border-gray-700 px-2.5 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
                            >
                              <RefreshCw className="h-3.5 w-3.5" />
                              Refresh
                            </button>
                          </div>

                          {runsLoading ? (
                            <div className="text-sm text-gray-400">Loading run history...</div>
                          ) : runsError ? (
                            <div className="rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
                              {runsError}
                            </div>
                          ) : runs.length === 0 ? (
                            <div className="text-sm text-gray-500">No runs recorded yet.</div>
                          ) : (
                            <div className="space-y-3">
                              {runs.map((run) => (
                                <div
                                  key={run.id}
                                  className="rounded-xl border border-gray-800 bg-gray-900/70 p-4"
                                >
                                  <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
                                    <div className="min-w-0 space-y-2">
                                      <div className="flex items-center gap-2">
                                        {statusIcon(run.status)}
                                        <span className="text-sm font-medium capitalize text-white">
                                          {run.status}
                                        </span>
                                        <span className="text-xs text-gray-500">
                                          Run #{run.id}
                                        </span>
                                      </div>
                                      <div className="grid grid-cols-1 gap-2 text-xs text-gray-400 sm:grid-cols-3">
                                        <span>Started: {formatDate(run.started_at)}</span>
                                        <span>Finished: {formatDate(run.finished_at)}</span>
                                        <span>Duration: {formatDuration(run.duration_ms)}</span>
                                      </div>
                                    </div>
                                    {run.output && (
                                      <pre className="max-h-40 w-full overflow-auto rounded-lg bg-gray-950 p-3 text-xs text-gray-300 whitespace-pre-wrap break-words lg:w-[28rem]">
                                        {run.output}
                                      </pre>
                                    )}
                                  </div>
                                </div>
                              ))}
                            </div>
                          )}
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
