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
      <div className="flex items-center gap-2 px-4 py-3 text-xs" style={{ color: 'var(--color-text-muted)' }}>
        <div className="animate-spin rounded-full h-4 w-4 border" style={{ borderColor: 'var(--color-glow-blue)', borderTopColor: 'var(--color-accent-blue)' }} />
        Loading run history...
      </div>
    );
  }

  if (error) {
    return (
      <div className="px-4 py-3">
        <div className="flex items-center justify-between">
          <span className="text-xs" style={{ color: 'var(--color-status-error)' }}>
            Failed to load run history: {error}
          </span>
          <button
            onClick={fetchRuns}
            className="hover:opacity-80 transition-colors duration-300"
            style={{ color: 'var(--color-text-muted)' }}
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
        <span className="text-xs" style={{ color: 'var(--color-text-muted)' }}>No runs recorded yet.</span>
        <button
          onClick={fetchRuns}
          className="hover:opacity-80 transition-colors duration-300"
          style={{ color: 'var(--color-text-muted)' }}
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>
    );
  }

  return (
    <div className="px-4 py-3">
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs font-medium" style={{ color: 'var(--color-text-secondary)' }}>
          Recent Runs ({runs.length})
        </span>
        <button
          onClick={fetchRuns}
          className="hover:opacity-80 transition-colors duration-300"
          title="Refresh runs"
          style={{ color: 'var(--color-text-muted)' }}
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="space-y-1.5 max-h-60 overflow-y-auto">
        {runs.map((run) => (
          <div
            key={run.id}
            className="rounded-lg px-3 py-2 text-xs border"
            style={{ backgroundColor: 'var(--color-bg-secondary)', opacity: 0.5, borderColor: 'var(--color-border-subtle)' }}
          >
            <div className="flex items-center justify-between mb-1">
              <div className="flex items-center gap-2">
                {run.status === 'ok' ? (
                  <CheckCircle className="h-3.5 w-3.5" style={{ color: 'var(--color-status-success)' }} />
                ) : (
                  <XCircle className="h-3.5 w-3.5" style={{ color: 'var(--color-status-error)' }} />
                )}
                <span className="capitalize" style={{ color: 'var(--color-text-secondary)' }}>{run.status}</span>
              </div>
              <span style={{ color: 'var(--color-text-muted)' }}>
                {formatDuration(run.duration_ms)}
              </span>
            </div>
            <div className="flex items-center gap-3" style={{ color: 'var(--color-text-muted)' }}>
              <span>{formatDate(run.started_at)}</span>
            </div>
            {run.output && (
              <pre className="mt-1.5 rounded p-2 text-xs overflow-x-auto max-h-24 whitespace-pre-wrap break-words" style={{ backgroundColor: 'var(--color-bg-primary)', color: 'var(--color-text-secondary)' }}>
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
        return <CheckCircle className="h-4 w-4" style={{ color: 'var(--color-status-success)' }} />;
      case 'error':
      case 'failed':
        return <XCircle className="h-4 w-4" style={{ color: 'var(--color-status-error)' }} />;
      default:
        return <AlertCircle className="h-4 w-4" style={{ color: 'var(--color-status-warning)' }} />;
    }
  };

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-xl p-4" style={{ backgroundColor: 'var(--color-status-error)', opacity: 0.1, border: '1px solid var(--color-status-error)', color: 'var(--color-status-error)' }}>
          Failed to load cron jobs: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--color-glow-blue)', borderTopColor: 'var(--color-accent-blue)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Clock className="h-5 w-5" style={{ color: 'var(--color-accent-blue)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-primary)' }}>
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

      {showForm && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="glass-card p-6 w-full max-w-md mx-4 animate-fade-in-scale">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold" style={{ color: 'var(--color-text-primary)' }}>Add Cron Job</h3>
              <button
                onClick={() => {
                  setShowForm(false);
                  setFormError(null);
                }}
                className="hover:opacity-80 transition-colors duration-300"
                style={{ color: 'var(--color-text-muted)' }}
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {formError && (
              <div className="mb-4 rounded-xl p-3 text-sm animate-fade-in" style={{ backgroundColor: 'var(--color-status-error)', opacity: 0.1, border: '1px solid var(--color-status-error)', color: 'var(--color-status-error)' }}>
                {formError}
              </div>
            )}

            <div className="space-y-4">
              <div>
                <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--color-text-secondary)' }}>
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
                <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--color-text-secondary)' }}>
                  Schedule <span style={{ color: 'var(--color-status-error)' }}>*</span>
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
                <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--color-text-secondary)' }}>
                  Command <span style={{ color: 'var(--color-status-error)' }}>*</span>
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
                className="px-4 py-2 text-sm font-medium border rounded-xl transition-all duration-300 hover:opacity-80"
                style={{ 
                  color: 'var(--color-text-secondary)', 
                  borderColor: 'var(--color-border-default)',
                  backgroundColor: 'var(--color-accent-blue)',
                  opacity: 0.05
                }}
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

      {jobs.length === 0 ? (
        <div className="glass-card p-8 text-center">
          <Clock className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--color-border-default)' }} />
          <p style={{ color: 'var(--color-text-muted)' }}>No scheduled tasks configured.</p>
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
                    <td className="px-4 py-3 font-mono text-xs" style={{ color: 'var(--color-text-muted)' }}>
                      <button
                        onClick={() =>
                          setExpandedJob((prev) =>
                            prev === job.id ? null : job.id,
                          )
                        }
                        className="flex items-center gap-1 hover:opacity-80 transition-colors duration-300"
                        style={{ color: 'var(--color-text-muted)' }}
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
                    <td className="px-4 py-3 font-medium text-sm" style={{ color: 'var(--color-text-primary)' }}>
                      {job.name ?? '-'}
                    </td>
                    <td className="px-4 py-3 font-mono text-xs max-w-[200px] truncate" style={{ color: 'var(--color-text-secondary)' }}>
                      {job.command}
                    </td>
                    <td className="px-4 py-3 text-xs" style={{ color: 'var(--color-text-muted)' }}>
                      {formatDate(job.next_run)}
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex items-center gap-1.5">
                        {statusIcon(job.last_status)}
                        <span className="text-xs capitalize" style={{ color: 'var(--color-text-secondary)' }}>
                          {job.last_status ?? '-'}
                        </span>
                      </div>
                    </td>
                    <td className="px-4 py-3">
                      <span
                        className="inline-flex items-center px-2.5 py-0.5 rounded-full text-xs font-semibold border"
                        style={{ 
                          color: job.enabled ? 'var(--color-status-success)' : 'var(--color-text-muted)',
                          borderColor: job.enabled ? 'var(--color-status-success)' : 'var(--color-border-default)',
                          backgroundColor: job.enabled ? 'var(--color-status-success)' : 'transparent',
                          opacity: job.enabled ? 0.1 : 1
                        }}
                      >
                        {job.enabled ? 'Enabled' : 'Disabled'}
                      </span>
                    </td>
                    <td className="px-4 py-3 text-right">
                      {confirmDelete === job.id ? (
                        <div className="flex items-center justify-end gap-2 animate-fade-in">
                          <span className="text-xs" style={{ color: 'var(--color-status-error)' }}>Delete?</span>
                          <button
                            onClick={() => handleDelete(job.id)}
                            className="text-xs font-medium hover:opacity-80"
                            style={{ color: 'var(--color-status-error)' }}
                          >
                            Yes
                          </button>
                          <button
                            onClick={() => setConfirmDelete(null)}
                            className="text-xs font-medium hover:opacity-80"
                            style={{ color: 'var(--color-text-muted)' }}
                          >
                            No
                          </button>
                        </div>
                      ) : (
                        <button
                          onClick={() => setConfirmDelete(job.id)}
                          className="hover:opacity-80 transition-all duration-300"
                          style={{ color: 'var(--color-text-muted)' }}
                        >
                          <Trash2 className="h-4 w-4" />
                        </button>
                      )}
                    </td>
                  </tr>
                  {expandedJob === job.id && (
                    <tr>
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
