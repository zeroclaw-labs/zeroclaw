// Detail view for a single SOP run. The Runs page is the list; this page is
// the run: live overlay via the shared useRunOverlay hook, approve/deny
// gates, the run-tinted canvas, and per-step captured calls.
// Route: /runs/:sop/:runId.
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link, useParams } from 'react-router-dom';
import { ArrowLeft, Check, Loader2, X } from 'lucide-react';
import {
  decideSop,
  getSop,
  getSopGraph,
  overlayStateByStep,
  runStatusBadge,
  type Sop,
  type SopGraph,
} from '@/lib/sops';
import { useRunOverlay } from '@/hooks/useRunOverlay';
import { t } from '@/lib/i18n';
import { Badge, Card, PageHeader } from '@/components/ui';
import SopStepList from '@/components/SopStepList';
import SopCanvas from './SopCanvas';

function noop() {}

export default function RunDetail() {
  const { sop = '', runId = '' } = useParams();
  const [graph, setGraph] = useState<SopGraph | null>(null);
  const [viewSop, setViewSop] = useState<Sop | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [deciding, setDeciding] = useState(false);
  const [decideError, setDecideError] = useState<string | null>(null);
  const { overlay, error: overlayError, setOverlay } = useRunOverlay(sop, runId);

  useEffect(() => {
    if (!sop) return;
    let active = true;
    Promise.all([getSopGraph(sop), getSop(sop)])
      .then(([g, full]) => {
        if (!active) return;
        setGraph(g);
        setViewSop(full);
      })
      .catch((e: unknown) => {
        if (active) setLoadError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, [sop]);

  const runStateByStep = useMemo(() => overlayStateByStep(overlay), [overlay]);

  const handleDecide = useCallback(
    (approve: boolean) => {
      if (!sop || !runId) return;
      setDeciding(true);
      setDecideError(null);
      decideSop(sop, runId, approve ? 'approve' : { deny: {} })
        .then(setOverlay)
        .catch((e: unknown) =>
          setDecideError(e instanceof Error ? e.message : String(e)),
        )
        .finally(() => setDeciding(false));
    },
    [sop, runId, setOverlay],
  );

  const gated = overlay ? overlay.waiting || overlay.paused : false;
  const error = loadError ?? overlayError;

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <PageHeader
        title={sop}
        description={runId}
        actions={
          <Link
            to="/runs"
            className="inline-flex items-center gap-1 rounded border border-pc-border px-2 py-1 text-sm text-pc-text hover:bg-pc-elevated"
          >
            <ArrowLeft className="h-4 w-4" aria-hidden /> {t('run_detail.back')}
          </Link>
        }
      />

      {error ? (
        <Card>
          <div className="text-status-error">{error}</div>
        </Card>
      ) : null}

      {overlay ? (
        <Card className="flex flex-wrap items-center gap-3 p-3">
          <Badge tone={runStatusBadge(overlay.status)}>
            {t(`sops.run_status.${overlay.status}`)}
          </Badge>
          <span className="tabular-nums text-sm text-pc-text-secondary">
            {t('run_detail.progress')} {overlay.current_step}/{overlay.total_steps}
          </span>
          <Link
            to={`/sops/${encodeURIComponent(sop)}`}
            className="text-sm text-pc-accent hover:underline"
          >
            {t('run_detail.open_sop')}
          </Link>
          {gated ? (
            <div className="ml-auto flex items-center gap-2">
              <span className="text-sm text-status-warning">
                {t('run_detail.gate_pending')}
              </span>
              <button
                type="button"
                disabled={deciding}
                onClick={() => handleDecide(true)}
                className="inline-flex items-center gap-1 rounded bg-pc-accent px-3 py-1.5 text-sm font-medium text-[#0b1220] hover:opacity-90 disabled:opacity-40"
              >
                {deciding ? (
                  <Loader2 className="h-4 w-4 animate-spin" aria-hidden />
                ) : (
                  <Check className="h-4 w-4" aria-hidden />
                )}
                {t('sops.approve')}
              </button>
              <button
                type="button"
                disabled={deciding}
                onClick={() => handleDecide(false)}
                className="inline-flex items-center gap-1 rounded border border-pc-border px-3 py-1.5 text-sm text-status-error hover:bg-pc-elevated disabled:opacity-40"
              >
                <X className="h-4 w-4" aria-hidden />
                {t('sops.deny')}
              </button>
            </div>
          ) : null}
          {decideError ? (
            <span className="w-full text-xs text-status-error">{decideError}</span>
          ) : null}
        </Card>
      ) : !error ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="h-5 w-5 animate-spin text-pc-text-muted" aria-hidden />
        </div>
      ) : null}

      {graph && viewSop ? (
        <SopCanvas
          draft={viewSop}
          graph={graph}
          selectedStep={null}
          runStateByStep={runStateByStep}
          readOnly
          onSelectStep={noop}
          onSelectTrigger={noop}
          onAddStep={noop}
          onConnect={noop}
          onDisconnect={noop}
          onConnectData={noop}
          onDisconnectData={noop}
        />
      ) : null}

      {graph && overlay ? <SopStepList graph={graph} overlay={overlay} showPins={false} /> : null}
    </div>
  );
}
