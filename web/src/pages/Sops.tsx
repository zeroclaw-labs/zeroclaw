import { useCallback, useEffect, useMemo, useState } from 'react';
import { AlertTriangle, XCircle, Loader2, ArrowDown } from 'lucide-react';
import { Badge, Card, PageHeader } from '@/components/ui';
import { t } from '@/lib/i18n';
import {
  listSops,
  getSopGraph,
  type SopSummary,
  type SopGraph,
  type GraphNode,
  type GraphPin,
  type GraphWire,
} from '@/lib/sops';

function pinTypeLabel(pin: GraphPin): string {
  if (pin.class === 'flow') return 'flow';
  return pin.data_type ?? 'any';
}

function wireRoleTone(wire: GraphWire): string {
  if (wire.class === 'data') return 'text-sky-500';
  switch (wire.flow_role) {
    case 'failure':
      return 'text-rose-500';
    case 'dependency':
      return 'text-amber-500';
    default:
      return 'text-emerald-500';
  }
}

function wireLabel(wire: GraphWire): string {
  if (wire.class === 'data') {
    return `${wire.from_pin ?? '?'} → ${wire.to_pin ?? '?'}`;
  }
  return wire.flow_role ?? 'sequence';
}

function NodeCard({ node }: { node: GraphNode }) {
  return (
    <div className="w-full max-w-xl rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface shadow-sm">
      <div className="flex items-center gap-2 border-b border-pc-border px-3 py-2">
        <span className="inline-flex h-6 w-6 items-center justify-center rounded bg-pc-accent-light text-xs font-semibold text-pc-accent">
          {node.step}
        </span>
        <span className="font-medium text-pc-text">{node.title}</span>
      </div>
      <div className="grid grid-cols-2 gap-3 px-3 py-2 text-xs">
        <div>
          <div className="mb-1 uppercase tracking-wide text-pc-text-muted">{t('sops.inputs')}</div>
          {node.inputs.length === 0 ? (
            <div className="text-pc-text-faint">—</div>
          ) : (
            node.inputs.map((pin) => (
              <div key={`in-${pin.name}`} className="flex items-center gap-1">
                <span
                  className={
                    pin.class === 'flow'
                      ? 'text-emerald-500'
                      : pin.required
                        ? 'text-sky-500'
                        : 'text-pc-text-faint'
                  }
                  aria-hidden
                >
                  ●
                </span>
                <span className="text-pc-text">{pin.name}</span>
                <span className="text-pc-text-muted">: {pinTypeLabel(pin)}</span>
                {pin.required && pin.class === 'data' ? (
                  <span className="text-rose-500">*</span>
                ) : null}
              </div>
            ))
          )}
        </div>
        <div className="text-right">
          <div className="mb-1 uppercase tracking-wide text-pc-text-muted">{t('sops.outputs')}</div>
          {node.outputs.length === 0 ? (
            <div className="text-pc-text-faint">—</div>
          ) : (
            node.outputs.map((pin) => (
              <div key={`out-${pin.name}`} className="flex items-center justify-end gap-1">
                <span className="text-pc-text">{pin.name}</span>
                <span className="text-pc-text-muted">: {pinTypeLabel(pin)}</span>
                <span
                  className={pin.class === 'flow' ? 'text-emerald-500' : 'text-sky-500'}
                  aria-hidden
                >
                  ●
                </span>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

function GraphCanvas({ graph }: { graph: SopGraph }) {
  const ordered = useMemo(
    () => [...graph.nodes].sort((a, b) => a.step - b.step),
    [graph.nodes],
  );
  const wiresByFrom = useMemo(() => {
    const map = new Map<number, GraphWire[]>();
    for (const w of graph.wires) {
      const list = map.get(w.from_step) ?? [];
      list.push(w);
      map.set(w.from_step, list);
    }
    return map;
  }, [graph.wires]);

  if (ordered.length === 0) {
    return <div className="text-pc-text-muted">{t('sops.empty_graph')}</div>;
  }

  return (
    <div className="flex flex-col items-center gap-1">
      {ordered.map((node, idx) => {
        const outbound = wiresByFrom.get(node.step) ?? [];
        return (
          <div key={node.step} className="flex w-full flex-col items-center">
            <NodeCard node={node} />
            {idx < ordered.length - 1 || outbound.length > 0 ? (
              <div className="flex flex-col items-center py-1">
                <ArrowDown className="h-4 w-4 text-pc-text-muted" aria-hidden />
                {outbound.map((w, i) => (
                  <span key={`w-${node.step}-${i}`} className={`text-[10px] ${wireRoleTone(w)}`}>
                    {node.step} → {w.to_step} [{wireLabel(w)}]
                  </span>
                ))}
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

function DiagnosticsPanel({ graph }: { graph: SopGraph }) {
  if (graph.diagnostics.length === 0) return null;
  return (
    <Card className="mt-4">
      <div className="mb-2 font-medium text-pc-text">{t('sops.diagnostics')}</div>
      <ul className="space-y-1 text-sm">
        {graph.diagnostics.map((d, i) => (
          <li key={i} className="flex items-start gap-2">
            {d.severity === 'error' ? (
              <XCircle className="mt-0.5 h-4 w-4 shrink-0 text-rose-500" aria-hidden />
            ) : (
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-500" aria-hidden />
            )}
            <span className="text-pc-text">
              <span className="text-pc-text-muted">
                {t('sops.step')} {d.step}:
              </span>{' '}
              {d.message}
            </span>
          </li>
        ))}
      </ul>
    </Card>
  );
}

export default function Sops() {
  const [sops, setSops] = useState<SopSummary[]>([]);
  const [selected, setSelected] = useState<string>('');
  const [graph, setGraph] = useState<SopGraph | null>(null);
  const [loading, setLoading] = useState(true);
  const [graphLoading, setGraphLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    listSops()
      .then((list) => {
        if (!active) return;
        setSops(list);
        const first = list[0];
        if (first) setSelected(first.name);
        setLoading(false);
      })
      .catch((e: unknown) => {
        if (!active) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      });
    return () => {
      active = false;
    };
  }, []);

  const loadGraph = useCallback((name: string) => {
    if (!name) return;
    setGraphLoading(true);
    getSopGraph(name)
      .then((g) => {
        setGraph(g);
        setGraphLoading(false);
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
        setGraph(null);
        setGraphLoading(false);
      });
  }, []);

  useEffect(() => {
    if (selected) loadGraph(selected);
  }, [selected, loadGraph]);

  return (
    <div className="space-y-4">
      <PageHeader title={t('sops.title')} description={t('sops.subtitle')} />
      {error ? (
        <Card>
          <div className="text-rose-500">{error}</div>
        </Card>
      ) : null}
      {loading ? (
        <Card>
          <Loader2 className="h-5 w-5 animate-spin text-pc-text-muted" aria-hidden />
        </Card>
      ) : sops.length === 0 ? (
        <Card>
          <div className="text-pc-text-muted">{t('sops.empty')}</div>
        </Card>
      ) : (
        <div className="grid grid-cols-[14rem_1fr] gap-4">
          <Card className="h-fit p-2">
            <ul className="space-y-1">
              {sops.map((s) => (
                <li key={s.name}>
                  <button
                    type="button"
                    onClick={() => setSelected(s.name)}
                    className={`w-full rounded px-2 py-1.5 text-left text-sm ${
                      s.name === selected
                        ? 'bg-pc-accent-light text-pc-accent'
                        : 'text-pc-text hover:bg-pc-elevated'
                    }`}
                  >
                    <div className="font-medium">{s.name}</div>
                    {s.description ? (
                      <div className="truncate text-xs text-pc-text-muted">{s.description}</div>
                    ) : null}
                  </button>
                </li>
              ))}
            </ul>
          </Card>
          <div>
            <div className="mb-3 flex items-center gap-2">
              <span className="font-medium text-pc-text">{selected}</span>
              {graph ? (
                <Badge tone="neutral">
                  {graph.nodes.length} {t('sops.steps')}
                </Badge>
              ) : null}
            </div>
            {graphLoading ? (
              <Loader2 className="h-5 w-5 animate-spin text-pc-text-muted" aria-hidden />
            ) : graph ? (
              <>
                <GraphCanvas graph={graph} />
                <DiagnosticsPanel graph={graph} />
              </>
            ) : null}
          </div>
        </div>
      )}
    </div>
  );
}
