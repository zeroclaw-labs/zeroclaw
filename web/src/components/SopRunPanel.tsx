import { useState } from 'react';
import { Link } from 'react-router-dom';
import { Check, Loader2, Square, X } from 'lucide-react';
import { Badge } from '@/components/ui';
import { CapturedCallList } from '@/components/SopCalls';
import {
  cancelSop,
  decideSop,
  getRunOverlay,
  isTerminalRunStatus,
  runStateBadge,
  runStatusBadge,
  type RunOverlay,
  type SopGraph,
  type SopRunSummary,
} from '@/lib/sops';
import { formatRelative } from '@/lib/format';
import { t } from '@/lib/i18n';

export function SopRunsPanel({
  runs,
  error,
  name,
  selectedRun,
  onSelect,
}: {
  runs: SopRunSummary[];
  error: string;
  name: string | null;
  selectedRun: string | null;
  onSelect: (sop: string, run: string) => void;
}) {
  const ordered = [...runs].sort((a, b) => b.started_at.localeCompare(a.started_at));
  const groups = [
    {
      title: 'sop_workspace.active_runs',
      items: ordered.filter((run) => run.active),
    },
    {
      title: 'sop_workspace.recent_runs',
      items: ordered.filter((run) => !run.active && run.sop_name === name).slice(0, 10),
    },
  ];
  return (
    <div className="space-y-6 p-4">
      {error && (
        <p role="alert" className="text-xs text-status-error">
          {t('workspace.runs_unavailable')} {error}
        </p>
      )}
      {groups.map(({ title, items }) => (
        <section key={title} className="space-y-2">
          <h2 className="text-xs font-medium text-pc-text-muted">
            {t(title)} <span className="ml-1 tabular-nums">{items.length}</span>
          </h2>
          {!items.length && !error && (
            <p className="py-2 text-xs text-pc-text-faint">{t('sop_workspace.no_runs')}</p>
          )}
          {items.map((run) => (
            <button
              key={run.run_id}
              type="button"
              aria-current={selectedRun === run.run_id ? 'true' : undefined}
              onClick={() => onSelect(run.sop_name, run.run_id)}
              className={`w-full space-y-2 rounded-xl border p-3 text-left hover:bg-pc-elevated ${selectedRun === run.run_id ? 'border-pc-accent/50 bg-pc-accent/5' : 'border-pc-border'}`}
            >
              <span className="block truncate text-sm font-medium">{run.sop_name}</span>
              <span className="flex flex-wrap items-center justify-between gap-2">
                <Badge tone={runStatusBadge(run.status)}>
                  {t(`sops.run_status.${run.status}`)}
                </Badge>
                <span className="text-xs text-pc-text-muted">
                  {run.current_step}/{run.total_steps}
                </span>
              </span>
              <span className="block truncate text-xs text-pc-text-muted">
                {formatRelative(run.started_at)} · {run.run_id.slice(0, 8)}
              </span>
            </button>
          ))}
        </section>
      ))}
      <Link to="/runs" className="inline-block text-xs text-pc-accent underline">
        {t('run_detail.back')}
      </Link>
    </div>
  );
}

export function SopRunControls({
  overlay,
  onUpdate,
}: {
  overlay: RunOverlay;
  onUpdate: (overlay: RunOverlay) => void;
}) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const act = async (action: 'approve' | 'deny' | 'stop') => {
    setPending(true);
    setError('');
    try {
      if (action === 'stop') {
        const result = await cancelSop(overlay.sop_name, overlay.run_id);
        // An accepted cancellation is authoritative even if the refresh fails.
        onUpdate({ ...overlay, status: result.status, waiting: false, paused: false });
        try {
          onUpdate(await getRunOverlay(overlay.sop_name, overlay.run_id));
        } catch {
          setError(t('sops.stop_refresh_error'));
        }
      } else {
        onUpdate(
          await decideSop(
            overlay.sop_name,
            overlay.run_id,
            action === 'approve' ? 'approve' : { deny: {} },
          ),
        );
      }
    } catch (e) {
      setError(
        action === 'stop' ? t('sops.stop_error') : e instanceof Error ? e.message : String(e),
      );
    } finally {
      setPending(false);
    }
  };
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone={runStatusBadge(overlay.status)}>
          {t(`sops.run_status.${overlay.status}`)}
        </Badge>
        <span className="mr-auto text-xs text-pc-text-muted">
          {t('run_detail.progress')} {overlay.current_step}/{overlay.total_steps}
        </span>
        {(overlay.waiting || overlay.paused) && (
          <>
            <button
              type="button"
              disabled={pending}
              onClick={() => act('approve')}
              className="btn-primary inline-flex items-center gap-1.5 px-3 py-2 text-xs disabled:opacity-40"
            >
              <Check className="h-3.5 w-3.5" />
              {t('sops.approve')}
            </button>
            <button
              type="button"
              disabled={pending}
              onClick={() => act('deny')}
              className="btn-secondary inline-flex items-center gap-1.5 px-3 py-2 text-xs disabled:opacity-40"
            >
              <X className="h-3.5 w-3.5" />
              {t('sops.deny')}
            </button>
          </>
        )}
        {!isTerminalRunStatus(overlay.status) && (
          <button
            type="button"
            disabled={pending || overlay.status === 'cancel_requested'}
            onClick={() => act('stop')}
            className="btn-secondary inline-flex items-center gap-1.5 px-3 py-2 text-xs disabled:opacity-40"
          >
            {pending ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Square className="h-3.5 w-3.5" />
            )}
            {t(overlay.status === 'cancel_requested' ? 'sops.stopping' : 'sops.stop')}
          </button>
        )}
      </div>
      {(overlay.waiting || overlay.paused) && (
        <p className="text-xs text-status-warning">{t('run_detail.gate_pending')}</p>
      )}
      {error && (
        <p role="alert" className="text-xs text-status-error">
          {error}
        </p>
      )}
    </div>
  );
}

export function SopRunInspector({
  overlay,
  graph,
  selectedStep,
  onSelect,
}: {
  overlay: RunOverlay | null;
  graph: SopGraph | null;
  selectedStep: number | null;
  onSelect: (step: number) => void;
}) {
  const node = overlay?.nodes.find((item) => item.step === selectedStep);
  return (
    <div className="space-y-4 p-4">
      <label className="block text-xs text-pc-text-muted">
        {t('sop_workspace.select_node')}
        <select
          value={selectedStep ?? ''}
          onChange={(e) => onSelect(Number(e.target.value))}
          className="mt-2 w-full rounded-lg border border-pc-border bg-pc-surface p-2 text-sm text-pc-text"
        >
          <option value="" disabled>
            {t('sops.inspector_empty')}
          </option>
          {graph?.nodes
            .filter((n) => n.kind === 'step')
            .map((n) => (
              <option key={n.step} value={n.step}>
                {n.step}. {n.title}
              </option>
            ))}
        </select>
      </label>
      {node && <Badge tone={runStateBadge(node.state)}>{t(`sops.run_state.${node.state}`)}</Badge>}
      <h3 className="text-sm font-medium">{t('sops.captured_calls')}</h3>
      {node?.tool_calls?.length ? (
        <CapturedCallList calls={node.tool_calls} />
      ) : (
        <p className="text-xs text-pc-text-muted">{t('sop_workspace.no_captured_calls')}</p>
      )}
    </div>
  );
}
