import React, { useState, useEffect, useCallback } from 'react';
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

function formatDuration(ms: number | null): string {
  if (ms === null || ms === undefined) return '-';
  if (ms < 1000) return `${ms}ms`;
  const secs = ms / 1000;
  if (secs < 60) return `${secs.toFixed(1)}s`;
  return `${(secs / 60).toFixed(1)}m`;
}

function RunHistoryPanel({ jobId }: { jobId: string }) {
  const [runs, setRuns] = useState<CronRun[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchRuns = useCallback(() => {
    setLoading(true);
    setError(null);
    getCronRuns(jobId, 20)
      .then(setRuns)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, [jobId]);

  useEffect(() => {
    fetchRuns();
  }, [fetchRuns]);

  if (loading) {
    return (
      <div className="flex items-center gap-2 px-4 py-3 text-[#556080] text-xs">
        <div className="animate-spin rounded-full h-4 w-4 border border-[#0080ff30] border-t-[#0080ff]" />
        Loading run history...
      </div>
    );
  }

  if (error) {
    return (
      <div className="px-4 py-3">
        <div className="flex items-center justify-between">
          <span className="text-xs text-[#ff6680]">
            Failed to load run history: {error}
          </span>
          <button
            onClick={fetchRuns}
            className="text-[#556080] hover:text-white transition-colors duration-300"
          >
            <RefreshCw className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
    );
  }

  if (runs.length === 0) {
    return (
      <div className="px-4 py-3 flex items-center justify-between">
        <span className="text-xs text-[#334060]">No runs recorded yet.</span>
        <button
          onClick={fetchRuns}
          className="text-[#556080] hover:text-white transition-colors duration-300"
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>
    );
  }

  return (
    <div className="px-4 py-3">
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs font-medium text-[#8892a8]">
          Recent Runs ({runs.length})
        </span>
        <button
          onClick={fetchRuns}
          className="text-[#556080] hover:text-white transition-colors duration-300"
          title="Refresh runs"
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="space-y-1.5 max-h-60 overflow-y-auto">
        {runs.map((run) => (
          <div
            key={run.id}
            className="bg-[#0a0a2060] rounded-lg px-3 py-2 text-xs border border-[#1a1a3e]/30"
          >
            <div className="flex items-center justify-between mb-1">
              <div className="flex items-center gap-2">
                {run.status === 'ok' ? (
                  <CheckCircle className="h-3.5 w-3.5 text-[#00e68a]" />
                ) : (
                  <XCircle className="h-3.5 w-3.5 text-[#ff4466]" />
                )}
                <span className="text-[#8892a8] capitalize">{run.status}</span>
              </div>
              <span className="text-[#556080]">
                {formatDuration(run.duration_ms)}
              </span>
            </div>
            <div className="flex items-center gap-3 text-[#556080]">
              <span>{formatDate(run.started_at)}</span>
            </div>
            {run.output && (
              <pre className="mt-1.5 bg-[#050510]/70 rounded p-2 text-[#8892a8] text-xs overflow-x-auto max-h-24 whitespace-pre-wrap break-words">
                {run.output}
              </pre>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

export default function Cron() {
  const [jobs, setJobs] = useState<CronJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [expandedJob, setExpandedJob] = useState<string | null>(null);

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
        return <CheckCircle className="h-4 w-4 text-[#00e68a]" />;
      case 'error':
      case 'failed':
        return <XCircle className="h-4 w-4 text-[#ff4466]" />;
      default:
        return <AlertCircle className="h-4 w-4 text-[#ffaa00]" />;
    }
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
              <tr>
                <th className="text-left">ID</th>
                <th className="text-left">Name</th>
                <th className="text-left">Command</th>
                <th className="text-left">Next Run</th>
                <th className="text-left">Last Status</th>
                <th className="text-left">Enabled</th>
                <th className="text-right">Actions</th>
              </tr>
            </thead>
            <tbody>
              {jobs.map((job) => (
                <React.Fragment key={job.id}>
                  <tr>
                    <td className="px-4 py-3 text-[#556080] font-mono text-xs">
                      <button
                        onClick={() =>
                          setExpandedJob((prev) =>
                            prev === job.id ? null : job.id,
                          )
                        }
                        className="flex items-center gap-1 text-[#556080] hover:text-white transition-colors duration-300"
                        title="Toggle run history"
                      >
                        {expandedJob === job.id ? (
                          <ChevronDown className="h-3.5 w-3.5" />
                        ) : (
                          <ChevronRight className="h-3.5 w-3.5" />
                        )}
                        {job.id.slice(0, 8)}
                      </button>
                    </td>
                    <td className="px-4 py-3 text-white font-medium text-sm">
                      {job.name ?? '-'}
                    </td>
                    <td className="px-4 py-3 text-[#8892a8] font-mono text-xs max-w-[200px] truncate">
                      {job.command}
                    </td>
                    <td className="px-4 py-3 text-[#556080] text-xs">
                      {formatDate(job.next_run)}
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex items-center gap-1.5">
                        {statusIcon(job.last_status)}
                        <span className="text-[#8892a8] text-xs capitalize">
                          {job.last_status ?? '-'}
                        </span>
                      </div>
                    </td>
                    <td className="px-4 py-3">
                      <span
                        className={`inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold border ${
                          job.enabled
                            ? 'text-[#00e68a] border-[#00e68a30]'
                            : 'text-[#334060] border-[#1a1a3e]'
                        }`}
                        style={{ background: job.enabled ? 'rgba(0,230,138,0.06)' : 'rgba(26,26,62,0.3)' }}
                      >
                        {job.enabled ? 'Enabled' : 'Disabled'}
                      </span>
                    </td>
                    <td className="px-4 py-3 text-right">
                      {confirmDelete === job.id ? (
                        <div className="flex items-center justify-end gap-2 animate-fade-in">
                          <span className="text-xs text-[#ff4466]">Delete?</span>
                          <button
                            onClick={() => handleDelete(job.id)}
                            className="text-[#ff4466] hover:text-[#ff6680] text-xs font-medium"
                          >
                            Yes
                          </button>
                          <button
                            onClick={() => setConfirmDelete(null)}
                            className="text-[#556080] hover:text-white text-xs font-medium"
                          >
                            No
                          </button>
                        </div>
                      ) : (
                        <button
                          onClick={() => setConfirmDelete(job.id)}
                          className="text-[#334060] hover:text-[#ff4466] transition-all duration-300"
                        >
                          <Trash2 className="h-4 w-4" />
                        </button>
                      )}
                    </td>
                  </tr>
                  {expandedJob === job.id && (
                    <tr className="bg-[#0a0a2080]">
                      <td colSpan={7}>
                        <RunHistoryPanel jobId={job.id} />
                      </td>
                    </tr>
                  )}
                </React.Fragment>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
