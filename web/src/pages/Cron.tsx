import type { CronSettings } from '@/lib/api';
import {
  addCronJob,
  ApiError,
  deleteCronJob,
  getCronJobs,
  getCronRuns,
  getCronSettings,
  getQuickstartState,
  patchCronJob,
  patchCronSettings,
  triggerCronJob,
} from '@/lib/api';
import { agentBoundChannels, type AgentBoundChannel } from '@/lib/agentChannels';
import { t } from '@/lib/i18n';
import { Badge, Button, Card, PageHeader } from '@/components/ui';
import ToolPicker from '@/components/ToolPicker';
import type { CronJob, CronRun } from '@/types/api';
import {
  AlertCircle,
  CheckCircle,
  ChevronDown,
  Clock,
  Pause,
  Pencil,
  Play,
  Plus,
  Power,
  RefreshCw,
  Trash2,
  X,
  XCircle,
} from 'lucide-react';
import React, { useCallback, useEffect, useState } from 'react';

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

function browserProvidedTimezone(): string {
  try {
    const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return typeof timezone === 'string' ? timezone : '';
  } catch {
    return '';
  }
}

function scheduleTimezone(job: CronJob): string | null {
  const schedule = job.schedule;
  if (schedule.kind === 'cron' && typeof schedule.tz === 'string' && schedule.tz.trim()) {
    return schedule.tz;
  }
  return null;
}

function describeCronSettingsError(err: unknown) {
  if (err instanceof ApiError) {
    return {
      name: err.name,
      status: err.status,
      code: err.envelope.code,
      path: err.envelope.path,
      op_index: err.envelope.op_index,
    };
  }

  if (err instanceof Error) {
    return { name: err.name };
  }

  return { type: typeof err };
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
      <div className="flex items-center gap-2 px-4 py-3 text-xs text-pc-text-muted">
        <div className="h-4 w-4 border-2 rounded-full animate-spin border-pc-border" style={{ borderTopColor: 'var(--pc-accent)' }} />
        {t('cron.loading_run_history')}
      </div>
    );
  }

  if (error) {
    return (
      <div className="px-4 py-3">
        <div className="flex items-center justify-between">
          <span className="text-xs text-status-error">
            {t('cron.load_run_history_error')}: {error}
          </span>
          <Button variant="ghost" size="sm" onClick={fetchRuns} aria-label={t('cron.recent_runs')}>
            <RefreshCw className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
    );
  }

  if (runs.length === 0) {
    return (
      <div className="px-4 py-3 flex items-center justify-between">
        <span className="text-xs text-pc-text-faint">{t('cron.no_runs')}</span>
        <Button variant="ghost" size="sm" onClick={fetchRuns} aria-label={t('cron.recent_runs')}>
          <RefreshCw className="h-3.5 w-3.5" />
        </Button>
      </div>
    );
  }

  return (
    <div className="px-4 py-3">
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs font-medium text-pc-text-secondary">
          {t('cron.recent_runs')} ({runs.length})
        </span>
        <Button variant="ghost" size="sm" onClick={fetchRuns} aria-label={t('cron.recent_runs')}>
          <RefreshCw className="h-3.5 w-3.5" />
        </Button>
      </div>
      <div className="space-y-1.5 max-h-60 overflow-y-auto">
        {runs.map((run) => (
          <div
            key={run.id}
            className="rounded-[var(--radius-md)] px-3 py-2 text-xs border border-pc-border bg-pc-elevated"
          >
            <div className="flex items-center justify-between mb-1">
              <div className="flex items-center gap-2">
                {run.status === 'ok' ? (
                  <CheckCircle className="h-3.5 w-3.5 text-status-success" />
                ) : (
                  <XCircle className="h-3.5 w-3.5 text-status-error" />
                )}
                <span className="text-pc-text-secondary">{run.status}</span>
              </div>
              <span className="text-pc-text-muted">
                {formatDuration(run.duration_ms)}
              </span>
            </div>
            <div className="flex items-center gap-3 text-pc-text-muted">
              <span>{formatDate(run.started_at)}</span>
            </div>
            {run.output && (
              <pre className="mt-1.5 rounded-[var(--radius-md)] p-2 text-xs overflow-x-auto max-h-24 whitespace-pre-wrap break-words font-mono bg-pc-code text-pc-text-secondary">
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
  const [toggling, setToggling] = useState<string | null>(null);
  const [triggerError, setTriggerError] = useState<string | null>(null);
  // Localized error for the pause/resume toggle. Kept SEPARATE from the
  // page-level `error` (which drives a full-page guard) so a single job's
  // toggle failure — or the "daemon ignored enabled" no-op path — renders
  // inline and leaves the jobs table / Add-Job button visible.
  const [toggleError, setToggleError] = useState<string | null>(null);
  const [runHistoryRefresh, setRunHistoryRefresh] = useState<Record<string, number>>({});
  const [settings, setSettings] = useState<CronSettings | null>(null);
  const [togglingCatchUp, setTogglingCatchUp] = useState(false);

  // Unified modal: null = closed, 'add' = adding, CronJob = editing
  const [modalJob, setModalJob] = useState<CronJob | 'add' | null>(null);

  // Shared form state for both add and edit
  const [formName, setFormName] = useState('');
  const [formSchedule, setFormSchedule] = useState('');
  const [formTimezone, setFormTimezone] = useState('');
  const [formCommand, setFormCommand] = useState('');
  const [formJobType, setFormJobType] = useState<'shell' | 'agent'>('shell');
  const [formPrompt, setFormPrompt] = useState('');
  const [formModel, setFormModel] = useState('');
  const [formSessionTarget, setFormSessionTarget] = useState<'isolated' | 'main'>('isolated');
  const [formAllowedTools, setFormAllowedTools] = useState('');
  const [formAgent, setFormAgent] = useState('');
  const [formDeliveryMode, setFormDeliveryMode] = useState<'none' | 'announce'>('none');
  const [formDeliveryChannel, setFormDeliveryChannel] = useState('');
  const [formDeliveryTo, setFormDeliveryTo] = useState('');
  const [formDeliveryBestEffort, setFormDeliveryBestEffort] = useState(true);
  const [agentOptions, setAgentOptions] = useState<string[]>([]);
  const [boundChannels, setBoundChannels] = useState<AgentBoundChannel[]>([]);
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const isEditing = modalJob !== null && modalJob !== 'add';

  const openAddModal = () => {
    setFormName('');
    setFormSchedule('');
    setFormTimezone(browserProvidedTimezone());
    setFormCommand('');
    setFormJobType('shell');
    setFormPrompt('');
    setFormModel('');
    setFormSessionTarget('isolated');
    setFormAllowedTools('');
    setFormAgent(agentOptions[0] ?? '');
    setFormDeliveryMode('none');
    setFormDeliveryChannel('');
    setFormDeliveryTo('');
    setFormDeliveryBestEffort(true);
    setFormError(null);
    setModalJob('add');
  };

  const openEditModal = (job: CronJob) => {
    const jobType = job.job_type === 'agent' ? 'agent' : 'shell';
    setFormName(job.name ?? '');
    setFormSchedule(job.expression);
    setFormTimezone(scheduleTimezone(job) ?? '');
    setFormJobType(jobType);
    setFormAgent((job as CronJob & { agent_alias?: string }).agent_alias ?? 'default');
    const delivery = job.delivery;
    if (delivery && (delivery.mode === 'announce' || delivery.mode === 'none')) {
      setFormDeliveryMode(delivery.mode);
      setFormDeliveryChannel(delivery.channel ?? '');
      setFormDeliveryTo(delivery.to ?? '');
      setFormDeliveryBestEffort(delivery.best_effort ?? true);
    } else {
      setFormDeliveryMode('none');
      setFormDeliveryChannel('');
      setFormDeliveryTo('');
      setFormDeliveryBestEffort(true);
    }
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
    getCronSettings().then(setSettings).catch((err) => {
      console.warn('[ZeroClaw] Failed to load cron settings:', describeCronSettingsError(err));
    });
  };

  const toggleCatchUp = async () => {
    if (!settings) return;
    setTogglingCatchUp(true);
    try {
      const updated = await patchCronSettings({
        catch_up_on_startup: !settings.catch_up_on_startup,
      });
      setSettings(updated);
    } catch (err: unknown) {
      console.warn('[ZeroClaw] Failed to update cron settings:', describeCronSettingsError(err));
    } finally {
      setTogglingCatchUp(false);
    }
  };

  useEffect(() => {
    fetchJobs();
    fetchSettings();
    void getQuickstartState()
      .then((opts) => {
        setAgentOptions(opts.agents);
        // Pre-seed the agent field with the first option so the
        // Add modal doesn't open with an empty required dropdown.
        setFormAgent((current) => current || opts.agents[0] || '');
      })
      .catch(() => {
        /* swallow: form will show an empty agent list */
      });
  }, []);

  // When the picked agent changes, refresh the bound-channels list so the
  // delivery-channel picker stays scoped to channels that agent owns.
  useEffect(() => {
    if (!formAgent) {
      setBoundChannels([]);
      return;
    }
    let cancelled = false;
    void agentBoundChannels(formAgent)
      .then((list) => {
        if (!cancelled) setBoundChannels(list);
      })
      .catch(() => {
        if (!cancelled) setBoundChannels([]);
      });
    return () => {
      cancelled = true;
    };
  }, [formAgent]);

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

    if (!isEditing && !formAgent.trim()) {
      setFormError(t('cron.agent_required_error'));
      setSubmitting(false);
      return;
    }
    // Delivery is only sent (and editable) on the add path; patchCronJob does
    // not accept it, so don't gate edits on the existing job's delivery config.
    if (!isEditing && formDeliveryMode === 'announce') {
      if (!formDeliveryChannel.trim()) {
        setFormError(t('cron.delivery_channel_required_error'));
        setSubmitting(false);
        return;
      }
      if (!formDeliveryTo.trim()) {
        setFormError(t('cron.delivery_target_required_error'));
        setSubmitting(false);
        return;
      }
    }

    try {
      if (isEditing) {
        const existingTimezone = scheduleTimezone(modalJob as CronJob);
        const timezone = formTimezone.trim();
        const patch: { agent: string; name?: string; schedule?: string; tz?: string; clear_tz?: boolean; command?: string; prompt?: string } = {
          // The gateway requires `agent` on every patch (it risk-gates a
          // command change); send the job's existing alias so a pure
          // name/schedule/prompt edit doesn't 422 with "missing field agent".
          agent: (modalJob as CronJob).agent_alias,
          name: formName.trim() || undefined,
          schedule: formSchedule.trim(),
        };
        if (timezone) {
          patch.tz = timezone;
        } else if (existingTimezone) {
          patch.clear_tz = true;
        }
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
          agent: formAgent.trim(),
          name: formName.trim() || undefined,
          schedule: formSchedule.trim(),
          job_type: formJobType,
        };
        const timezone = formTimezone.trim();
        if (timezone) body.tz = timezone;
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
        if (formDeliveryMode === 'announce') {
          body.delivery = {
            mode: 'announce',
            channel: formDeliveryChannel.trim(),
            to: formDeliveryTo.trim(),
            best_effort: formDeliveryBestEffort,
          };
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

  // Pause/resume a job without deleting it — toggles the existing `enabled`
  // flag the scheduler already honours.
  const handleToggleEnabled = async (job: CronJob) => {
    if (toggling === job.id) return;
    const desired = !job.enabled;
    setToggling(job.id);
    try {
      const updated = await patchCronJob(job.id, {
        agent: job.agent_alias,
        enabled: desired,
      });
      setJobs((prev) => prev.map((j) => (j.id === job.id ? updated : j)));
      // The gateway echoes the stored job. If `enabled` didn't move, this
      // daemon build predates pause/resume on the cron PATCH endpoint
      // (CronPatchBody has no `enabled` field, so the flag is silently
      // ignored). Say so rather than leaving the button looking broken.
      if (updated.enabled !== desired) {
        setToggleError(t('cron.pause_resume_unsupported_error'));
      } else {
        setToggleError(null);
      }
    } catch (err: unknown) {
      setToggleError(err instanceof Error ? err.message : t('cron.edit_error'));
    } finally {
      setToggling(null);
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
      <div className="p-6">
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-4 text-sm text-status-error">
          {t('cron.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin border-pc-border" style={{ borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full p-6 gap-6 overflow-hidden">
      {/* Header */}
      <PageHeader
        title={t('cron.scheduled_tasks')}
        actions={
          <Button variant="primary" size="md" onClick={openAddModal}>
            <Plus className="h-4 w-4" />{t('cron.add_job')}
          </Button>
        }
      />

      {/* Catch-up toggle */}
      {settings && (
        <Card className="px-4 py-3 flex items-center justify-between">
          <div>
            <span className="text-sm font-medium text-pc-text">
              {t('cron.catch_up_title')}
            </span>
            <p className="text-xs mt-0.5 text-pc-text-muted">
              {t('cron.catch_up_description')}
            </p>
          </div>
          <button
            type="button"
            onClick={toggleCatchUp}
            disabled={togglingCatchUp}
            aria-pressed={settings.catch_up_on_startup}
            className="relative inline-flex h-6 w-11 items-center rounded-full transition-colors duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base disabled:opacity-40 cursor-pointer"
            style={settings.catch_up_on_startup
              ? { background: 'var(--pc-accent)' }
              : { background: 'var(--pc-text-muted)' }
            }
          >
            <span
              className={`inline-block h-4 w-4 rounded-full bg-white transition-transform duration-200 ${settings.catch_up_on_startup
                  ? 'translate-x-6'
                  : 'translate-x-1'
                }`}
            />
          </button>
        </Card>
      )}

      {/* Unified Add / Edit Modal */}
      {modalJob !== null && (
        <div className="fixed inset-0 flex items-center justify-center z-50 p-4" style={{ background: 'var(--pc-overlay, rgba(0,0,0,0.5))' }}>
          <div className="bg-pc-surface border border-pc-border rounded-[var(--radius-lg)] shadow-[var(--pc-shadow-md)] p-6 w-full max-w-md mt-15 max-h-9/10 overflow-auto">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-base font-semibold text-pc-text">
                {isEditing ? t('cron.edit_modal_title') : t('cron.add_modal_title')}
              </h3>
              <Button variant="ghost" size="sm" onClick={closeModal} aria-label={t('cron.cancel')}>
                <X className="h-4 w-4" />
              </Button>
            </div>
            {formError && (
              <div className="mb-4 rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-3 text-sm text-status-error">
                {formError}
              </div>
            )}
            <div className="space-y-4">
              {/* Job Type Selector */}
              <div>
                <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                  {t('cron.job_type')}
                </label>
                {isEditing ? (
                  <span
                    className={[
                      'inline-flex items-center px-3 py-2 rounded-[var(--radius-md)] text-sm font-medium border',
                      formJobType === 'agent'
                        ? 'border-pc-accent/30 bg-pc-accent/10 text-pc-accent'
                        : 'border-pc-border text-pc-text-secondary',
                    ].join(' ')}
                  >
                    {t(formJobType === 'shell' ? 'cron.job_type_shell' : 'cron.job_type_agent')}
                  </span>
                ) : (
                  <div className="flex gap-2">
                    <button
                      type="button"
                      onClick={() => setFormJobType('shell')}
                      className={`flex-1 px-3 py-2.5 rounded-[var(--radius-md)] text-sm font-medium border transition-colors cursor-pointer ${formJobType === 'shell'
                          ? 'border-pc-accent text-pc-accent bg-pc-accent/10'
                          : 'border-pc-border text-pc-text-muted hover:bg-[var(--pc-hover)] hover:text-pc-text'
                        }`}
                    >
                      {t('cron.job_type_shell')}
                    </button>
                    <button
                      type="button"
                      onClick={() => setFormJobType('agent')}
                      className={`flex-1 px-3 py-2.5 rounded-[var(--radius-md)] text-sm font-medium border transition-colors cursor-pointer ${formJobType === 'agent'
                          ? 'border-pc-accent text-pc-accent bg-pc-accent/10'
                          : 'border-pc-border text-pc-text-muted hover:bg-[var(--pc-hover)] hover:text-pc-text'
                        }`}
                    >
                      {t('cron.job_type_agent')}
                    </button>
                  </div>
                )}
              </div>
              <div>
                <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                  {t('cron.agent_label')} {!isEditing && <span className="text-status-error">*</span>}
                </label>
                {isEditing ? (
                  // patchCronJob does NOT accept `agent`, so on edit this is a
                  // read-only display of the job's current agent — shown (not
                  // hidden) so the operator can see which agent owns the job.
                  <span className="inline-flex items-center px-3 py-2 rounded-[var(--radius-md)] text-sm font-medium border border-pc-border text-pc-text-secondary font-mono">
                    agents.{formAgent || '-'}
                  </span>
                ) : (
                  <>
                    <select
                      value={formAgent}
                      onChange={(e) => setFormAgent(e.target.value)}
                      className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2.5 text-sm appearance-none cursor-pointer"
                    >
                      {agentOptions.length === 0 ? (
                        <option value="">{t('cron.no_configured_agents')}</option>
                      ) : (
                        agentOptions.map((alias) => (
                          <option key={alias} value={alias}>
                            agents.{alias}
                          </option>
                        ))
                      )}
                    </select>
                    <p className="text-xs mt-1 text-pc-text-faint">
                      {t('cron.agent_help')}
                    </p>
                  </>
                )}
              </div>
              <div>
                <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                  {t('cron.name_optional')}
                </label>
                <input type="text" value={formName} onChange={(e) => setFormName(e.target.value)} placeholder={t('cron.name_placeholder')} className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2.5 text-sm" />
              </div>
              <div>
                <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                  {t('cron.schedule_required')} <span className="text-status-error">*</span>
                </label>
                <input type="text" value={formSchedule} onChange={(e) => setFormSchedule(e.target.value)} placeholder={t('cron.schedule_placeholder')} className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2.5 text-sm" />
              </div>
              <div>
                <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                  {t('cron.timezone')}
                </label>
                <input type="text" value={formTimezone} onChange={(e) => setFormTimezone(e.target.value)} placeholder={t('cron.timezone_placeholder')} className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2.5 text-sm font-mono" />
              </div>

              {/* Conditional fields based on job type */}
              {formJobType === 'shell' ? (
                <div>
                  <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                    {t('cron.command_required')} <span className="text-status-error">*</span>
                  </label>
                  <textarea
                    value={formCommand}
                    onChange={(e) => setFormCommand(e.target.value)}
                    placeholder={t('cron.command_placeholder')}
                    rows={4}
                    className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2.5 text-sm resize-y font-mono"
                  />
                </div>
              ) : (
                <>
                  <div>
                    <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                      {t('cron.prompt_required')} <span className="text-status-error">*</span>
                    </label>
                    <textarea
                      value={formPrompt}
                      onChange={(e) => setFormPrompt(e.target.value)}
                      placeholder={t('cron.prompt_placeholder')}
                      rows={4}
                      className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2.5 text-sm resize-y"
                    />
                  </div>
                  {/* Model / session-target / allowed-tools. patchCronJob does
                      NOT accept any of these, so on edit they render read-only
                      (populated from the job) rather than being hidden — the
                      operator can see the current config even though it's fixed
                      after creation. */}
                  {isEditing ? (
                    <>
                      <div>
                        <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                          {t('cron.model_optional')}
                        </label>
                        <span className="inline-flex items-center px-3 py-2 rounded-[var(--radius-md)] text-sm font-medium border border-pc-border text-pc-text-secondary font-mono">
                          {formModel.trim() || t('cron.model_default')}
                        </span>
                      </div>
                      <div>
                        <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                          {t('cron.session_target')}
                        </label>
                        <span className="inline-flex items-center px-3 py-2 rounded-[var(--radius-md)] text-sm font-medium border border-pc-border text-pc-text-secondary">
                          {t(formSessionTarget === 'main' ? 'cron.session_main' : 'cron.session_isolated')}
                        </span>
                      </div>
                      <div>
                        <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                          {t('cron.allowed_tools_optional')}
                        </label>
                        <span className="inline-flex items-center px-3 py-2 rounded-[var(--radius-md)] text-sm font-medium border border-pc-border text-pc-text-secondary font-mono break-all">
                          {formAllowedTools.trim() || t('cron.all_tools')}
                        </span>
                      </div>
                    </>
                  ) : (
                    <>
                      <div>
                        <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                          {t('cron.model_optional')}
                        </label>
                        <input
                          type="text"
                          value={formModel}
                          onChange={(e) => setFormModel(e.target.value)}
                          placeholder={t('cron.model_placeholder')}
                          className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2.5 text-sm"
                        />
                      </div>
                      <div>
                        <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                          {t('cron.session_target')}
                        </label>
                        <div className="flex gap-2">
                          <button
                            type="button"
                            onClick={() => setFormSessionTarget('isolated')}
                            className={`flex-1 px-3 py-2 rounded-[var(--radius-md)] text-xs font-medium border transition-colors cursor-pointer ${formSessionTarget === 'isolated'
                                ? 'border-pc-accent text-pc-accent bg-pc-accent/10'
                                : 'border-pc-border text-pc-text-muted hover:bg-[var(--pc-hover)] hover:text-pc-text'
                              }`}
                          >
                            {t('cron.session_isolated')}
                          </button>
                          <button
                            type="button"
                            onClick={() => setFormSessionTarget('main')}
                            className={`flex-1 px-3 py-2 rounded-[var(--radius-md)] text-xs font-medium border transition-colors cursor-pointer ${formSessionTarget === 'main'
                                ? 'border-pc-accent text-pc-accent bg-pc-accent/10'
                                : 'border-pc-border text-pc-text-muted hover:bg-[var(--pc-hover)] hover:text-pc-text'
                              }`}
                          >
                            {t('cron.session_main')}
                          </button>
                        </div>
                      </div>
                      <div>
                        <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                          {t('cron.allowed_tools_optional')}
                        </label>
                        {/* The form keeps `formAllowedTools` as the canonical
                            comma-joined string so the submit path (split on
                            ',' → parsedTools → body.allowed_tools) is
                            unchanged. The ToolPicker just reads that string as
                            string[] and writes the selection back joined with
                            ', '. */}
                        <ToolPicker
                          id="cron-allowed-tools"
                          value={formAllowedTools
                            .split(',')
                            .map((s) => s.trim())
                            .filter(Boolean)}
                          onChange={(next) => setFormAllowedTools(next.join(', '))}
                        />
                      </div>
                    </>
                  )}
                </>
              )}

              {/* Delivery section — scoped to the picked agent's channel
                  bindings. The channel composite + identity field
                  (matrix user_id, discord guild_ids, ...) come from the
                  agentBoundChannels helper so the operator sees exactly
                  which channel goes where. Dangling channel refs are
                  accepted on add; the scheduler logs loudly when a
                  dangling delivery fires. */}
              {isEditing ? (
                // patchCronJob does NOT accept `delivery`, so on edit the
                // delivery config renders read-only (populated from the job)
                // instead of being hidden.
                <div className="border-t border-pc-border pt-4">
                  <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                    {t('cron.delivery')}
                  </label>
                  {formDeliveryMode === 'announce' ? (
                    <div className="space-y-1.5 text-sm">
                      <div className="flex items-center gap-2">
                        <span className="text-pc-text-faint text-xs uppercase tracking-wider">{t('cron.delivery_mode')}</span>
                        <span className="text-pc-text-secondary font-medium">{t('cron.delivery_announce')}</span>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-pc-text-faint text-xs uppercase tracking-wider">{t('cron.delivery_channel')}</span>
                        <span className="text-pc-text-secondary font-mono break-all">{formDeliveryChannel || '-'}</span>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-pc-text-faint text-xs uppercase tracking-wider">{t('cron.delivery_to')}</span>
                        <span className="text-pc-text-secondary font-mono break-all">{formDeliveryTo || '-'}</span>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-pc-text-faint text-xs uppercase tracking-wider">{t('cron.delivery_best_effort')}</span>
                        <span className="text-pc-text-secondary">{formDeliveryBestEffort ? t('cron.yes') : t('cron.no')}</span>
                      </div>
                    </div>
                  ) : (
                    <span className="inline-flex items-center px-3 py-2 rounded-[var(--radius-md)] text-sm font-medium border border-pc-border text-pc-text-secondary">
                      {t('cron.delivery_none')}
                    </span>
                  )}
                  <p className="text-xs mt-3 text-pc-text-faint">
                    {t('cron.delivery_fixed_help')}
                  </p>
                </div>
              ) : (
                <div className="border-t border-pc-border pt-4">
                  <label className="block text-[11px] font-medium mb-1.5 uppercase tracking-wider text-pc-text-faint">
                    {t('cron.delivery')}
                  </label>
                  <div className="flex gap-2 mb-2">
                    <button
                      type="button"
                      onClick={() => setFormDeliveryMode('none')}
                      className={`flex-1 px-3 py-2 rounded-[var(--radius-md)] text-xs font-medium border transition-colors cursor-pointer ${formDeliveryMode === 'none'
                          ? 'border-pc-accent text-pc-accent bg-pc-accent/10'
                          : 'border-pc-border text-pc-text-muted hover:bg-[var(--pc-hover)] hover:text-pc-text'
                        }`}
                    >
                      {t('cron.delivery_none')}
                    </button>
                    <button
                      type="button"
                      onClick={() => setFormDeliveryMode('announce')}
                      className={`flex-1 px-3 py-2 rounded-[var(--radius-md)] text-xs font-medium border transition-colors cursor-pointer ${formDeliveryMode === 'announce'
                          ? 'border-pc-accent text-pc-accent bg-pc-accent/10'
                          : 'border-pc-border text-pc-text-muted hover:bg-[var(--pc-hover)] hover:text-pc-text'
                        }`}
                    >
                      {t('cron.delivery_announce')}
                    </button>
                  </div>
                  {formDeliveryMode === 'announce' && (
                    <div className="space-y-2">
                      <select
                        value={formDeliveryChannel}
                        onChange={(e) => setFormDeliveryChannel(e.target.value)}
                        className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2 text-sm appearance-none cursor-pointer"
                      >
                        <option value="">
                          {boundChannels.length === 0
                            ? t('cron.no_channels_bound')
                            : t('cron.select_channel')}
                        </option>
                        {boundChannels.map((ch) => (
                          <option key={ch.composite} value={ch.composite}>
                            {ch.composite}
                            {ch.identity ? ` — ${ch.identity}` : ''}
                          </option>
                        ))}
                      </select>
                      <input
                        type="text"
                        value={formDeliveryTo}
                        onChange={(e) => setFormDeliveryTo(e.target.value)}
                        placeholder={t('cron.delivery_to_placeholder')}
                        className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 w-full px-3 py-2 text-sm font-mono"
                      />
                      <label className="flex items-center gap-2 text-xs text-pc-text-muted">
                        <input
                          type="checkbox"
                          checked={formDeliveryBestEffort}
                          onChange={(e) => setFormDeliveryBestEffort(e.target.checked)}
                          className="accent-pc-accent"
                        />
                        {t('cron.delivery_best_effort_label')}
                      </label>
                      <p className="text-xs text-pc-text-faint">
                        {t('cron.delivery_channels_from')} <code className="font-mono text-pc-text-secondary">agents.{formAgent || '<agent>'}.channels</code>.
                        {' '}{t('cron.delivery_channels_warn')}
                      </p>
                    </div>
                  )}
                </div>
              )}
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <Button variant="ghost" size="md" onClick={closeModal}>
                {t('cron.cancel')}
              </Button>
              <Button variant="primary" size="md" onClick={handleSubmit} disabled={submitting}>
                {submitting
                  ? t(isEditing ? 'cron.saving' : 'cron.adding')
                  : t(isEditing ? 'cron.save' : 'cron.add_job')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* Inline trigger-error banner — keeps the cron table mounted on failed manual runs */}
      {triggerError && (
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-3 text-sm text-status-error flex items-start justify-between gap-3">
          <span className="whitespace-pre-wrap break-words">{triggerError}</span>
          <Button variant="ghost" size="sm" className="shrink-0" onClick={() => setTriggerError(null)} aria-label={t('cron.dismiss')}>
            <X className="h-4 w-4" />
          </Button>
        </div>
      )}

      {/* Inline pause/resume-error banner — keeps the cron table mounted on a failed
          toggle or when the daemon silently ignores the `enabled` change */}
      {toggleError && (
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-3 text-sm text-status-error flex items-start justify-between gap-3">
          <span className="whitespace-pre-wrap break-words">{toggleError}</span>
          <Button variant="ghost" size="sm" className="shrink-0" onClick={() => setToggleError(null)} aria-label={t('cron.dismiss')}>
            <X className="h-4 w-4" />
          </Button>
        </div>
      )}

      {/* Jobs Table */}
      {jobs.length === 0 ? (
        <Card className="p-10 text-center">
          <Clock className="h-10 w-10 mx-auto mb-3 text-pc-text-faint" />
          <p className="text-sm text-pc-text-muted">{t('cron.empty')}</p>
        </Card>
      ) : (
        <Card padded={false} className="overflow-auto flex-1 min-h-0">
          <table className="w-full text-sm border-collapse">
            <thead>
              <tr className="border-b border-pc-border text-[11px] font-medium uppercase tracking-wider text-pc-text-faint">
                <th className="px-4 py-2.5 text-left font-medium">{t('cron.id')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.name')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.job_type')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.command')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.timezone')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.next_run')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.last_status')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.enabled')}</th>
                <th className="px-4 py-2.5 text-center font-medium">{t('cron.actions')}</th>
              </tr>
            </thead>
            <tbody>
              {jobs.map((job) => (
                <React.Fragment key={job.id}>
                  <tr className="border-b border-pc-border/60 last:border-0">
                    <td className="px-4 py-2.5 max-w-44">
                      <div className="flex min-w-0 flex-col items-start gap-1.5">
                        <span
                          className="min-w-0 max-w-full truncate font-mono text-xs text-pc-text-secondary"
                          title={job.id}
                        >
                          {job.id}
                        </span>
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() =>
                            setExpandedJob((prev) =>
                              prev === job.id ? null : job.id,
                            )
                          }
                          aria-expanded={expandedJob === job.id}
                          title={t('cron.show_recent_runs')}
                        >
                          <ChevronDown
                            className={`h-3.5 w-3.5 shrink-0 transition-transform duration-150 ${
                              expandedJob === job.id ? 'rotate-180' : ''
                            }`}
                          />
                          {t('cron.run_history')}
                        </Button>
                      </div>
                    </td>
                    <td className="px-4 py-2.5 font-medium text-center text-pc-text">
                      {job.name ?? '-'}
                    </td>
                    <td className="px-4 py-2.5 text-center">
                      <Badge tone={job.job_type === 'agent' ? 'ok' : 'neutral'}>
                        {job.job_type === 'agent' ? t('cron.job_type_agent') : t('cron.job_type_shell')}
                      </Badge>
                    </td>
                    <td className="px-4 py-2.5 font-mono text-xs max-w-50 truncate text-center text-pc-text-secondary">
                      {job.prompt ?? job.command}
                    </td>
                    <td className="px-4 py-2.5 font-mono text-xs text-center text-pc-text-muted">
                      {scheduleTimezone(job) ?? t('cron.runtime_local_timezone')}
                    </td>
                    <td className="px-4 py-2.5 text-xs text-center text-pc-text-muted">
                      {formatDate(job.next_run)}
                    </td>
                    <td className="px-4 py-2.5 text-center">
                      <div className="flex items-center gap-1.5 justify-center">
                        {statusIcon(job.last_status)}
                        <span className="text-xs capitalize text-pc-text-secondary">
                          {job.last_status ?? '-'}
                        </span>
                      </div>
                    </td>
                    <td className="px-4 py-2.5 text-center">
                      <Badge tone={job.enabled ? 'ok' : 'neutral'}>
                        {job.enabled ? t('cron.enabled_status') : t('cron.disabled_status')}
                      </Badge>
                    </td>
                    <td className="px-4 py-2.5 text-center">
                      <div className="flex items-center justify-center gap-1">
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => handleTrigger(job.id)}
                          title={t('cron.trigger')}
                          aria-label={t('cron.trigger')}
                          disabled={triggering === job.id}
                        >
                          {triggering === job.id ? (
                            <RefreshCw className="h-4 w-4 animate-spin" />
                          ) : (
                            <Play className="h-4 w-4" />
                          )}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => handleToggleEnabled(job)}
                          title={job.enabled ? t('cron.pause') : t('cron.resume')}
                          aria-label={job.enabled ? t('cron.pause') : t('cron.resume')}
                          disabled={toggling === job.id}
                        >
                          {toggling === job.id ? (
                            <RefreshCw className="h-4 w-4 animate-spin" />
                          ) : job.enabled ? (
                            <Pause className="h-4 w-4" />
                          ) : (
                            <Power className="h-4 w-4" />
                          )}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => openEditModal(job)}
                          title={t('cron.edit')}
                          aria-label={t('cron.edit')}
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                        {confirmDelete === job.id ? (
                          <div className="flex items-center justify-end gap-2">
                            <span className="text-xs text-status-error">
                              {t('cron.confirm_delete')}
                            </span>
                            <button
                              type="button"
                              onClick={() => handleDelete(job.id)}
                              className="text-xs font-medium text-status-error cursor-pointer hover:underline"
                            >
                              {t('cron.yes')}
                            </button>
                            <button
                              type="button"
                              onClick={() => setConfirmDelete(null)}
                              className="text-xs font-medium text-pc-text-muted cursor-pointer hover:text-pc-text"
                            >
                              {t('cron.no')}
                            </button>
                          </div>
                        ) : (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setConfirmDelete(job.id)}
                            title={t('cron.delete')}
                            aria-label={t('cron.delete')}
                          >
                            <Trash2 className="h-4 w-4" />
                          </Button>
                        )}
                      </div>
                    </td>
                  </tr>
                  {expandedJob === job.id && (
                    <tr>
                      <td colSpan={9} className="bg-pc-elevated border-b border-pc-border">
                        <RunHistoryPanel jobId={job.id} refreshKey={runHistoryRefresh[job.id] ?? 0} />
                      </td>
                    </tr>
                  )}
                </React.Fragment>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </div>
  );
}
