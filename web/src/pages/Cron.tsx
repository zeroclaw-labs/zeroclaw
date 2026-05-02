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
  Pencil,
  Play,
} from 'lucide-react';
import type { CronJob, CronRun } from '@/types/api';
import {
  getCronJobs,
  addCronJob,
  deleteCronJob,
  getCronRuns,
  getCronSettings,
  patchCronSettings,
  patchCronJob,
  triggerCronJob,
} from '@/lib/api';
import type { CronSettings } from '@/lib/api';
import { t } from '@/lib/i18n';

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

function RunHistoryPanel({ jobId, refreshKey = 0 }: { jobId: string; refreshKey?: number }) {
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

  useEffect(() => { fetchRuns(); }, [fetchRuns, refreshKey]);

  if (loading) {
    return (
      <div className="flex items-center gap-2 px-4 py-3 text-xs" style={{ color: 'var(--pc-text-muted)' }}>
        <div className="h-4 w-4 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
        Loading run history...
      </div>
    );
  }

  if (error) {
    return (
      <div className="px-4 py-3">
        <div className="flex items-center justify-between">
          <span className="text-xs" style={{ color: 'var(--color-status-error)' }}>
            {t('cron.load_run_history_error')}: {error}
          </span>
          <button
            onClick={fetchRuns}
            className="btn-icon">
            <RefreshCw className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
    );
  }

  if (runs.length === 0) {
    return (
      <div className="px-4 py-3 flex items-center justify-between">
        <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>{t('cron.no_runs')}</span>
        <button
          onClick={fetchRuns}
          className="btn-icon"
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>
    );
  }

  return (
    <div className="px-4 py-3">
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs font-medium" style={{ color: 'var(--pc-text-secondary)' }}>
          {t('cron.recent_runs')} ({runs.length})
        </span>
        <button
          onClick={fetchRuns}
          className="btn-icon"
          title="Refresh runs"
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className="space-y-1.5 max-h-60 overflow-y-auto">
        {runs.map((run) => (
          <div
            key={run.id}
            className="rounded-xl px-3 py-2 text-xs border" style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)' }}
          >
            <div className="flex items-center justify-between mb-1">
              <div className="flex items-center gap-2">
                {run.status === 'ok' ? (
                  <CheckCircle className="h-3.5 w-3.5" style={{ color: 'var(--color-status-success)' }} />
                ) : (
                  <XCircle className="h-3.5 w-3.5" style={{ color: 'var(--color-status-error)' }} />
                )}
                <span style={{ color: 'var(--pc-text-secondary)' }}>{run.status}</span>
              </div>
              <span style={{ color: 'var(--pc-text-muted)' }}>
                {formatDuration(run.duration_ms)}
              </span>
            </div>
            <div className="flex items-center gap-3" style={{ color: 'var(--pc-text-muted)' }}>
              <span>{formatDate(run.started_at)}</span>
            </div>
            {run.output && (
              <pre className="mt-1.5 rounded-lg p-2 text-xs overflow-x-auto max-h-24 whitespace-pre-wrap break-words font-mono" style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-secondary)' }}>
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
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [expandedJob, setExpandedJob] = useState<string | null>(null);
  const [triggering, setTriggering] = useState<string | null>(null);
  const [triggerError, setTriggerError] = useState<string | null>(null);
  const [runHistoryRefresh, setRunHistoryRefresh] = useState<Record<string, number>>({});
  const [settings, setSettings] = useState<CronSettings | null>(null);
  const [togglingCatchUp, setTogglingCatchUp] = useState(false);

  // Unified modal: null = closed, 'add' = adding, CronJob = editing
  const [modalJob, setModalJob] = useState<CronJob | 'add' | null>(null);

  // Shared form state for both add and edit
  const [formName, setFormName] = useState('');
  const [formSchedule, setFormSchedule] = useState('');
  const [formCommand, setFormCommand] = useState('');
  const [formJobType, setFormJobType] = useState<'shell' | 'agent'>('shell');
  const [formPrompt, setFormPrompt] = useState('');
  const [formModel, setFormModel] = useState('');
  const [formSessionTarget, setFormSessionTarget] = useState<'isolated' | 'main'>('isolated');
  const [formAllowedTools, setFormAllowedTools] = useState('');
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const isEditing = modalJob !== null && modalJob !== 'add';

  const openAddModal = () => {
    setFormName('');
    setFormSchedule('');
    setFormCommand('');
    setFormJobType('shell');
    setFormPrompt('');
    setFormModel('');
    setFormSessionTarget('isolated');
    setFormAllowedTools('');
    setFormError(null);
    setModalJob('add');
  };

  const openEditModal = (job: CronJob) => {
    const jobType = job.job_type === 'agent' ? 'agent' : 'shell';
    setFormName(job.name ?? '');
    setFormSchedule(job.expression);
    setFormJobType(jobType);
    if (jobType === 'agent') {
      setFormPrompt(job.prompt ?? '');
      setFormCommand('');
      setFormModel(job.model ?? '');
      setFormSessionTarget(
        job.session_target === 'main' ? 'main' : 'isolated',
      );
      setFormAllowedTools(
        job.allowed_tools ? job.allowed_tools.join(', ') : '',
      );
    } else {
      setFormCommand(job.command);
      setFormPrompt('');
      setFormModel('');
      setFormSessionTarget('isolated');
      setFormAllowedTools('');
    }
    setFormError(null);
    setModalJob(job);
  };

  const closeModal = () => {
    setModalJob(null);
    setFormError(null);
  };

  const fetchJobs = () => {
    setLoading(true);
    getCronJobs().then(setJobs).catch((err) => setError(err.message)).finally(() => setLoading(false));
  };

  const fetchSettings = () => {
    getCronSettings().then(setSettings).catch(() => {});
  };

  const toggleCatchUp = async () => {
    if (!settings) return;
    setTogglingCatchUp(true);
    try {
      const updated = await patchCronSettings({
        catch_up_on_startup: !settings.catch_up_on_startup,
      });
      setSettings(updated);
    } catch {
      // silently fail — user can retry
    } finally {
      setTogglingCatchUp(false);
    }
  };

  useEffect(() => {
    fetchJobs();
    fetchSettings();
  }, []);

  const handleSubmit = async () => {
    const isAgent = formJobType === 'agent';
    if (!formSchedule.trim()) {
      setFormError(t('cron.validation_error'));
      return;
    }
    if (isAgent && !formPrompt.trim()) {
      setFormError(t('cron.prompt_required_error'));
      return;
    }
    if (!isAgent && !formCommand.trim()) {
      setFormError(t('cron.command_required_error'));
      return;
    }
    setSubmitting(true);
    setFormError(null);

    try {
      if (isEditing) {
        const patch: { name?: string; schedule?: string; command?: string; prompt?: string } = {
          name: formName.trim() || undefined,
          schedule: formSchedule.trim(),
        };
        if (isAgent) {
          patch.prompt = formPrompt.trim();
        } else {
          patch.command = formCommand.trim();
        }
        const updated = await patchCronJob(
          (modalJob as CronJob).id,
          patch,
        );
        setJobs((prev) => prev.map((j) => (j.id === updated.id ? updated : j)));
      } else {
        const body: Parameters<typeof addCronJob>[0] = {
          name: formName.trim() || undefined,
          schedule: formSchedule.trim(),
          job_type: formJobType,
        };
        if (isAgent) {
          body.prompt = formPrompt.trim();
          if (formModel.trim()) body.model = formModel.trim();
          body.session_target = formSessionTarget;
          const parsedTools = formAllowedTools
            .split(',')
            .map((s) => s.trim())
            .filter(Boolean);
          if (parsedTools.length > 0) body.allowed_tools = parsedTools;
        } else {
          body.command = formCommand.trim();
        }
        const job = await addCronJob(body);
        setJobs((prev) => [...prev, job]);
      }
      closeModal();
    } catch (err: unknown) {
      setFormError(
        err instanceof Error
          ? err.message
          : t(isEditing ? 'cron.edit_error' : 'cron.add_error'),
      );
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await deleteCronJob(id);
      setJobs((prev) => prev.filter((j) => j.id !== id));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : t('cron.delete_error'));
    } finally {
      setConfirmDelete(null);
    }
  };

  const handleTrigger = async (id: string) => {
    setTriggering(id);
    setTriggerError(null);
    try {
      const result = await triggerCronJob(id);
      // Refresh job list so last_run / last_status reflect the manual run.
      try {
        const refreshed = await getCronJobs();
        setJobs(refreshed);
      } catch {
        // If list refresh fails, leave the existing rows; the user can reload.
      }
      // Auto-expand the run history so the user can see the result they just triggered,
      // and bump its refresh key so an already-expanded panel reloads.
      setExpandedJob(id);
      setRunHistoryRefresh((prev) => ({ ...prev, [id]: (prev[id] ?? 0) + 1 }));
      if (!result.success) {
        const detail = result.output?.trim();
        setTriggerError(detail ? `${t('cron.trigger_error')}: ${detail}` : t('cron.trigger_error'));
      }
    } catch (err: unknown) {
      setTriggerError(err instanceof Error ? err.message : t('cron.trigger_error'));
    } finally {
      setTriggering(null);
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
        <div className="rounded-2xl border p-4" style={{ background: 'var(--color-status-error-alpha-08)', borderColor: 'var(--color-status-error-alpha-20)', color: 'var(--color-status-error)' }}>
          {t('cron.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full p-6 gap-6 animate-fade-in overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Clock className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
            {t('cron.scheduled_tasks')} ({jobs.length})
          </h2>
        </div>
        <button
          onClick={openAddModal}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          <Plus className="h-4 w-4" />{t('cron.add_job')}
        </button>
      </div>

      {/* Catch-up toggle */}
      {settings && (
        <div className="glass-card px-4 py-3 flex items-center justify-between">
          <div>
            <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
              Catch up missed jobs on startup
            </span>
            <p className="text-xs mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>
              Run all overdue jobs when ZeroClaw starts after downtime
            </p>
          </div>
          <button
            onClick={toggleCatchUp}
            disabled={togglingCatchUp}
            className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors duration-300 focus:outline-none`}
            style={settings.catch_up_on_startup
              ? { background: 'var(--color-status-info)' }
              : { background: 'var(--pc-bg-elevated)', border: '1px solid var(--pc-border)' }
            }
          >
            <span
              className={`inline-block h-4 w-4 rounded-full bg-white transition-transform duration-300 ${
                settings.catch_up_on_startup
                  ? 'translate-x-6'
                  : 'translate-x-1'
              }`}
            />
          </button>
        </div>
      )}

      {/* Unified Add / Edit Modal */}
      {modalJob !== null && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="surface-panel p-6 w-full max-w-md mx-4 animate-fade-in-scale mt-15">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
                {isEditing ? t('cron.edit_modal_title') : t('cron.add_modal_title')}
              </h3>
              <button
                onClick={closeModal}
                className="btn-icon"
              >
                <X className="h-5 w-5" />
              </button>
            </div>
            {formError && (
              <div className="mb-4 rounded-xl border p-3 text-sm animate-fade-in" style={{ background: 'var(--color-status-error-alpha-08)', borderColor: 'var(--color-status-error-alpha-20)', color: 'var(--color-status-error)' }}>
                {formError}
              </div>
            )}
            <div className="space-y-4">
              {/* Job Type Selector */}
              <div>
                <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                  {t('cron.job_type')}
                </label>
                {isEditing ? (
                  <span
                    className="inline-flex items-center px-3 py-2 rounded-xl text-sm font-medium border"
                    style={formJobType === 'agent'
                      ? { color: 'var(--pc-accent)', borderColor: 'rgba(0, 128, 255, 0.2)', background: 'rgba(0, 128, 255, 0.06)' }
                      : { color: 'var(--pc-text-secondary)', borderColor: 'var(--pc-border)', background: 'transparent' }}
                  >
                    {t(formJobType === 'shell' ? 'cron.job_type_shell' : 'cron.job_type_agent')}
                  </span>
                ) : (
                  <div className="flex gap-2">
                    <button
                      type="button"
                      onClick={() => setFormJobType('shell')}
                      className={`flex-1 px-3 py-2.5 rounded-xl text-sm font-medium border transition-colors ${
                        formJobType === 'shell'
                          ? 'border-[var(--pc-accent)] text-[var(--pc-accent)]'
                          : 'border-[var(--pc-border)] text-[var(--pc-text-muted)]'
                      }`}
                      style={formJobType === 'shell' ? { background: 'rgba(0, 128, 255, 0.08)' } : { background: 'transparent' }}
                    >
                      {t('cron.job_type_shell')}
                    </button>
                    <button
                      type="button"
                      onClick={() => setFormJobType('agent')}
                      className={`flex-1 px-3 py-2.5 rounded-xl text-sm font-medium border transition-colors ${
                        formJobType === 'agent'
                          ? 'border-[var(--pc-accent)] text-[var(--pc-accent)]'
                          : 'border-[var(--pc-border)] text-[var(--pc-text-muted)]'
                      }`}
                      style={formJobType === 'agent' ? { background: 'rgba(0, 128, 255, 0.08)' } : { background: 'transparent' }}
                    >
                      {t('cron.job_type_agent')}
                    </button>
                  </div>
                )}
              </div>
              <div>
                <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                  {t('cron.name_optional')}
                </label>
                <input type="text" value={formName} onChange={(e) => setFormName(e.target.value)} placeholder="e.g. Daily cleanup" className="input-electric w-full px-3 py-2.5 text-sm" />
              </div>
              <div>
                <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                  {t('cron.schedule_required')} <span style={{ color: 'var(--color-status-error)' }}>*</span>
                </label>
                <input type="text" value={formSchedule} onChange={(e) => setFormSchedule(e.target.value)} placeholder="e.g. 0 0 * * * (cron expression)" className="input-electric w-full px-3 py-2.5 text-sm" />
              </div>

              {/* Conditional fields based on job type */}
              {formJobType === 'shell' ? (
                <div>
                  <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                    {t('cron.command_required')} <span style={{ color: 'var(--color-status-error)' }}>*</span>
                  </label>
                  <textarea
                    value={formCommand}
                    onChange={(e) => setFormCommand(e.target.value)}
                    placeholder="e.g. cleanup --older-than 7d"
                    rows={4}
                    className="input-electric w-full px-3 py-2.5 text-sm resize-y font-mono"
                  />
                </div>
              ) : (
                <>
                  <div>
                    <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                      {t('cron.prompt_required')} <span style={{ color: 'var(--color-status-error)' }}>*</span>
                    </label>
                    <textarea
                      value={formPrompt}
                      onChange={(e) => setFormPrompt(e.target.value)}
                      placeholder={t('cron.prompt_placeholder')}
                      rows={4}
                      className="input-electric w-full px-3 py-2.5 text-sm resize-y"
                    />
                  </div>
                  {!isEditing && (
                    <>
                      <div>
                        <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                          {t('cron.model_optional')}
                        </label>
                        <input
                          type="text"
                          value={formModel}
                          onChange={(e) => setFormModel(e.target.value)}
                          placeholder={t('cron.model_placeholder')}
                          className="input-electric w-full px-3 py-2.5 text-sm"
                        />
                      </div>
                      <div>
                        <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                          {t('cron.session_target')}
                        </label>
                        <div className="flex gap-2">
                          <button
                            type="button"
                            onClick={() => setFormSessionTarget('isolated')}
                            className={`flex-1 px-3 py-2 rounded-xl text-xs font-medium border transition-colors ${
                              formSessionTarget === 'isolated'
                                ? 'border-[var(--pc-accent)] text-[var(--pc-accent)]'
                                : 'border-[var(--pc-border)] text-[var(--pc-text-muted)]'
                            }`}
                            style={formSessionTarget === 'isolated' ? { background: 'rgba(0, 128, 255, 0.08)' } : { background: 'transparent' }}
                          >
                            {t('cron.session_isolated')}
                          </button>
                          <button
                            type="button"
                            onClick={() => setFormSessionTarget('main')}
                            className={`flex-1 px-3 py-2 rounded-xl text-xs font-medium border transition-colors ${
                              formSessionTarget === 'main'
                                ? 'border-[var(--pc-accent)] text-[var(--pc-accent)]'
                                : 'border-[var(--pc-border)] text-[var(--pc-text-muted)]'
                            }`}
                            style={formSessionTarget === 'main' ? { background: 'rgba(0, 128, 255, 0.08)' } : { background: 'transparent' }}
                          >
                            {t('cron.session_main')}
                          </button>
                        </div>
                      </div>
                      <div>
                        <label className="block text-xs font-semibold mb-1.5 uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
                          {t('cron.allowed_tools_optional')}
                        </label>
                        <input
                          type="text"
                          value={formAllowedTools}
                          onChange={(e) => setFormAllowedTools(e.target.value)}
                          placeholder={t('cron.allowed_tools_placeholder')}
                          className="input-electric w-full px-3 py-2.5 text-sm font-mono"
                        />
                      </div>
                    </>
                  )}
                </>
              )}
            </div>
            <div className="flex justify-end gap-3 mt-6">
              <button
                onClick={closeModal}
                className="btn-secondary px-4 py-2 text-sm font-medium"
              >
                {t('cron.cancel')}
              </button>
              <button
                onClick={handleSubmit}
                disabled={submitting}
                className="btn-electric px-4 py-2 text-sm font-medium"
              >
                {submitting
                  ? t(isEditing ? 'cron.saving' : 'cron.adding')
                  : t(isEditing ? 'cron.save' : 'cron.add_job')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Inline trigger-error banner — keeps the cron table mounted on failed manual runs */}
      {triggerError && (
        <div
          className="rounded-2xl border p-3 text-sm flex items-start justify-between gap-3 animate-fade-in"
          style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}
        >
          <span className="whitespace-pre-wrap break-words">{triggerError}</span>
          <button
            onClick={() => setTriggerError(null)}
            className="btn-icon shrink-0"
            title={t('cron.dismiss')}
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      {/* Jobs Table */}
      {jobs.length === 0 ? (
        <div className="card p-8 text-center">
          <Clock className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
          <p style={{ color: 'var(--pc-text-muted)' }}>{t('cron.empty')}</p>
        </div>
      ) : (
        <div className="card overflow-auto rounded-2xl flex-1 min-h-0">
          <table className="table-electric">
            <thead>
              <tr>
                <th>{t('cron.id')}</th>
                <th>{t('cron.name')}</th>
                <th>{t('cron.job_type')}</th>
                <th>{t('cron.command')}</th>
                <th>{t('cron.next_run')}</th>
                <th>{t('cron.last_status')}</th>
                <th>{t('cron.enabled')}</th>
                <th className="text-right">{t('cron.actions')}</th>
              </tr>
            </thead>
            <tbody>
              {jobs.map((job) => (
                <React.Fragment key={job.id}>
                  <tr>
                    <td className="font-mono text-xs">
                      <button
                        onClick={() =>
                          setExpandedJob((prev) =>
                            prev === job.id ? null : job.id,
                          )
                      }
                        className="flex items-center gap-1 btn-icon"
                        title="Toggle run history"
                      >
                        {expandedJob === job.id ? (
                          <ChevronDown className="h-3.5 w-3.5" />
                        ) : (
                          <ChevronRight className="h-3.5 w-3.5" />
                        )}
                        {job.id?.slice(0, 8) ?? job.id}
                      </button>
                    </td>
                    <td className="font-medium text-sm" style={{ color: 'var(--pc-text-primary)' }}>
                      {job.name ?? '-'}
                    </td>
                    <td>
                      <span className="inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold border"
                        style={job.job_type === 'agent'
                          ? { color: 'var(--pc-accent)', borderColor: 'rgba(0, 128, 255, 0.2)', background: 'rgba(0, 128, 255, 0.06)' }
                          : { color: 'var(--pc-text-secondary)', borderColor: 'var(--pc-border)', background: 'transparent' }
                        }>
                        {job.job_type === 'agent' ? t('cron.job_type_agent') : t('cron.job_type_shell')}
                      </span>
                    </td>
                    <td className="font-mono text-xs max-w-[200px] truncate" style={{ color: 'var(--pc-text-secondary)' }}>
                      {job.prompt ?? job.command}
                    </td>
                    <td className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                      {formatDate(job.next_run)}
                    </td>
                    <td>
                      <div className="flex items-center gap-1.5">
                        {statusIcon(job.last_status)}
                        <span className="text-xs capitalize" style={{ color: 'var(--pc-text-secondary)' }}>
                          {job.last_status ?? '-'}
                        </span>
                      </div>
                    </td>
                    <td>
                      <span className="inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold border"
                        style={job.enabled ? { color: 'var(--color-status-success)', borderColor: 'rgba(0, 230, 138, 0.2)', background: 'rgba(0, 230, 138, 0.06)' } : { color: 'var(--pc-text-faint)', borderColor: 'var(--pc-border)', background: 'transparent' }}>
                        {job.enabled ? t('cron.enabled_status') : t('cron.disabled_status')}
                      </span>
                    </td>
                    <td className="text-right">
                      <div className="flex items-center justify-end gap-2">
                        <button
                          onClick={() => handleTrigger(job.id)}
                          className="btn-icon"
                          title={t('cron.trigger')}
                          disabled={triggering === job.id}
                        >
                          {triggering === job.id ? (
                            <RefreshCw className="h-4 w-4 animate-spin" />
                          ) : (
                            <Play className="h-4 w-4" />
                          )}
                        </button>
                        <button
                          onClick={() => openEditModal(job)}
                          className="btn-icon"
                          title={t('cron.edit')}
                        >
                          <Pencil className="h-4 w-4" />
                        </button>
                        {confirmDelete === job.id ? (
                          <div className="flex items-center justify-end gap-2 animate-fade-in">
                            <span className="text-xs" style={{ color: 'var(--color-status-error)' }}>
                              {t('cron.confirm_delete')}
                            </span>
                            <button
                              onClick={() => handleDelete(job.id)}
                              className="text-xs font-medium"
                              style={{ color: 'var(--color-status-error)' }}
                            >
                              {t('cron.yes')}
                            </button>
                            <button
                              onClick={() => setConfirmDelete(null)}
                              className="text-xs font-medium"
                              style={{ color: 'var(--pc-text-muted)' }}
                            >
                              {t('cron.no')}
                            </button>
                          </div>
                        ) : (
                          <button
                            onClick={() => setConfirmDelete(job.id)}
                            className="btn-icon"
                          >
                            <Trash2 className="h-4 w-4" />
                          </button>
                        )}
                      </div>
                    </td>
                  </tr>
                  {expandedJob === job.id && (
                    <tr>
                      <td colSpan={8} style={{ background: 'var(--pc-bg-elevated)' }}>
                        <RunHistoryPanel jobId={job.id} refreshKey={runHistoryRefresh[job.id] ?? 0} />
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
