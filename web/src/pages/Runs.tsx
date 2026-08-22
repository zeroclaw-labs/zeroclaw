import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { Activity, ExternalLink, Loader2, Square } from 'lucide-react';
import { cancelSop, isTerminalRunStatus, runStatusBadge, type SopRunSummary } from '@/lib/sops';
import { basePath } from '@/lib/basePath';
import { getToken } from '@/lib/auth';
import { formatRelative } from '@/lib/format';
import { t } from '@/lib/i18n';
import { Badge, Card, PageHeader } from '@/components/ui';
import { confirmsCancellation, runsStreamEffect } from './runs.logic';

type RunsFrame =
  | { type: 'snapshot'; runs: SopRunSummary[] }
  | { type: 'run'; run: SopRunSummary }
  | { type: 'disabled' }
  | { type: 'lagged'; missed: number }
  | { type: 'error'; error: string };

function count(n: number): string {
  return (n === 1 ? t('runs.count_one') : t('runs.count_other')).replace('{n}', String(n));
}

function sortRuns(map: Map<string, SopRunSummary>): SopRunSummary[] {
  return [...map.values()].sort((a, b) => b.started_at.localeCompare(a.started_at));
}

export default function Runs() {
  const [runs, setRuns] = useState<Map<string, SopRunSummary>>(new Map());
  const [connected, setConnected] = useState(false);
  const [ready, setReady] = useState(false);
  const [disabled, setDisabled] = useState(false);
  const [activeOnly, setActiveOnly] = useState(false);
  const [cancellingIds, setCancellingIds] = useState<Set<string>>(new Set());
  const [cancelErrors, setCancelErrors] = useState<Map<string, string>>(new Map());
  const wsRef = useRef<WebSocket | null>(null);

  useEffect(() => {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}${basePath || ''}/ws/sops/runs`;
    const token = getToken();
    const protocols = token ? ['zeroclaw.v1', `bearer.${token}`] : ['zeroclaw.v1'];

    let stopped = false;
    let retry: number | undefined;

    const connect = () => {
      if (stopped) return;
      const ws = new WebSocket(url, protocols);
      wsRef.current = ws;

      ws.onopen = () => setConnected(true);
      ws.onclose = () => {
        setConnected(false);
        if (!stopped) retry = window.setTimeout(connect, 2000);
      };
      ws.onerror = () => ws.close();

      ws.onmessage = (event) => {
        let frame: RunsFrame;
        try {
          frame = JSON.parse(event.data) as RunsFrame;
        } catch {
          return;
        }
        const effect = runsStreamEffect(frame.type);
        if (effect === 'replace' && frame.type === 'snapshot') {
          const next = new Map<string, SopRunSummary>();
          for (const r of frame.runs) next.set(r.run_id, r);
          setRuns(next);
          setCancelErrors((prev) => {
            const nextErrors = new Map(prev);
            for (const run of frame.runs) {
              if (confirmsCancellation(run.status, isTerminalRunStatus(run.status))) {
                nextErrors.delete(run.run_id);
              }
            }
            return nextErrors;
          });
          setReady(true);
        } else if (effect === 'upsert' && frame.type === 'run') {
          setRuns((prev) => {
            const next = new Map(prev);
            next.set(frame.run.run_id, frame.run);
            return next;
          });
          if (
            confirmsCancellation(frame.run.status, isTerminalRunStatus(frame.run.status))
          ) {
            setCancelErrors((prev) => {
              const next = new Map(prev);
              next.delete(frame.run.run_id);
              return next;
            });
          }
        } else if (effect === 'disable') {
          setDisabled(true);
          setReady(true);
        } else if (effect === 'resync') {
          // The missed frame may be the only terminal transition for a row.
          // Reconnect so the server sends a fresh authoritative snapshot.
          setReady(false);
          ws.close();
        }
      };
    };

    connect();
    return () => {
      stopped = true;
      if (retry) window.clearTimeout(retry);
      wsRef.current?.close();
      wsRef.current = null;
    };
  }, []);

  const shown = useMemo(() => {
    const list = sortRuns(runs);
    return activeOnly ? list.filter((r) => r.active) : list;
  }, [runs, activeOnly]);

  // Reconcile the row from the HTTP response immediately. The subsequent live
  // frame remains authoritative, but the operator should see CancelRequested
  // (or a terminal race winner) without waiting for WebSocket delivery.
  const handleCancel = useCallback((sopName: string, runId: string) => {
    setCancellingIds((prev) => new Set(prev).add(runId));
    setCancelErrors((prev) => {
      const next = new Map(prev);
      next.delete(runId);
      return next;
    });
    cancelSop(sopName, runId)
      .then((result) => {
        setRuns((prev) => {
          const next = new Map(prev);
          next.set(runId, result.run);
          return next;
        });
      })
      .catch((e: unknown) => {
        console.error('cancelSop failed', e);
        setCancelErrors((prev) => new Map(prev).set(runId, t('sops.stop_error')));
      })
      .finally(() => {
        setCancellingIds((prev) => {
          const next = new Set(prev);
          next.delete(runId);
          return next;
        });
      });
  }, []);

  return (
    <div className="space-y-4">
      <PageHeader
        title={t('runs.title')}
        description={t('runs.subtitle')}
        actions={
          <div className="flex items-center gap-3">
            <label className="flex items-center gap-1.5 text-xs text-pc-text-secondary">
              <input
                type="checkbox"
                checked={activeOnly}
                onChange={(e) => setActiveOnly(e.target.checked)}
                className="accent-pc-accent"
              />
              {t('runs.active_only')}
            </label>
            <span className="inline-flex items-center gap-1 text-xs text-pc-text-muted">
              <Activity
                className={`h-3.5 w-3.5 ${connected ? 'text-status-success' : 'text-pc-text-muted'}`}
                aria-hidden
              />
              {t('runs.live')}
            </span>
            <span className="text-xs text-pc-text-muted">{count(shown.length)}</span>
          </div>
        }
      />

      {disabled ? (
        <Card className="p-8 text-center text-sm text-pc-text-muted">{t('runs.disabled')}</Card>
      ) : !ready ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="h-5 w-5 animate-spin text-pc-text-muted" aria-hidden />
        </div>
      ) : shown.length === 0 ? (
        <Card className="p-8 text-center text-sm text-pc-text-muted">{t('runs.empty')}</Card>
      ) : (
        <Card className="overflow-hidden p-0">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-pc-border text-left text-xs uppercase tracking-wide text-pc-text-muted">
                <th className="px-4 py-2.5 font-medium">{t('runs.col_sop')}</th>
                <th className="px-4 py-2.5 font-medium">{t('runs.col_status')}</th>
                <th className="px-4 py-2.5 font-medium">{t('runs.col_progress')}</th>
                <th className="px-4 py-2.5 font-medium">{t('runs.col_trigger')}</th>
                <th className="px-4 py-2.5 font-medium">{t('runs.col_started')}</th>
                <th className="px-4 py-2.5 font-medium">{t('runs.col_run_id')}</th>
                <th className="px-4 py-2.5" />
              </tr>
            </thead>
            <tbody className="divide-y divide-pc-border">
              {shown.map((r) => (
                <tr key={r.run_id} className="hover:bg-pc-elevated/50">
                  <td className="px-4 py-2.5 font-medium text-pc-text">{r.sop_name}</td>
                  <td className="px-4 py-2.5">
                    <Badge tone={runStatusBadge(r.status)}>
                      {t(`sops.run_status.${r.status}`)}
                    </Badge>
                  </td>
                  <td className="px-4 py-2.5 tabular-nums text-pc-text-secondary">
                    {r.current_step}/{r.total_steps}
                  </td>
                  <td className="px-4 py-2.5 text-pc-text-secondary">{r.trigger_source}</td>
                  <td className="px-4 py-2.5 text-pc-text-muted">{formatRelative(r.started_at)}</td>
                  <td
                    className="px-4 py-2.5 font-mono text-xs text-pc-text-muted"
                    title={r.run_id}
                  >
                    {r.run_id.slice(0, 8)}
                  </td>
                  <td className="px-4 py-2.5 text-right">
                    <div className="flex flex-wrap items-center justify-end gap-2">
                      {!isTerminalRunStatus(r.status) ? (
                        <button
                          type="button"
                          disabled={
                            cancellingIds.has(r.run_id) || r.status === 'cancel_requested'
                          }
                          onClick={() => handleCancel(r.sop_name, r.run_id)}
                          className="inline-flex items-center gap-1 rounded border border-status-error/25 bg-status-error/10 px-2 py-1 text-xs text-status-error hover:bg-status-error/15 disabled:opacity-40"
                        >
                          {cancellingIds.has(r.run_id) || r.status === 'cancel_requested' ? (
                            <Loader2 className="h-3 w-3 animate-spin" aria-hidden />
                          ) : (
                            <Square className="h-3 w-3" aria-hidden fill="currentColor" />
                          )}
                          {r.status === 'cancel_requested'
                            ? t('sops.stopping')
                            : t('sops.stop')}
                        </button>
                      ) : null}
                      <Link
                        to={`/runs/${encodeURIComponent(r.sop_name)}/${encodeURIComponent(r.run_id)}`}
                        className="inline-flex items-center gap-1 text-xs text-pc-accent hover:underline"
                      >
                        {t('runs.open')}
                        <ExternalLink className="h-3 w-3" aria-hidden />
                      </Link>
                      {cancelErrors.get(r.run_id) ? (
                        <span className="w-full text-xs text-status-error">
                          {cancelErrors.get(r.run_id)}
                        </span>
                      ) : null}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </div>
  );
}
