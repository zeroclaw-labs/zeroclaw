import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AlertTriangle, XCircle, Loader2, ArrowDown, Plus, Save, Trash2, X } from 'lucide-react';
import { Badge, Card, PageHeader } from '@/components/ui';
import SopCanvas from './SopCanvas';
import { t } from '@/lib/i18n';
import {
  listSops,
  getSopGraph,
  getRunOverlay,
  getSop,
  createSop,
  saveSop,
  deleteSop,
  wireDraft,
  type WireRole,
  type SopSummary,
  type SopGraph,
  type GraphNode,
  type GraphPin,
  type GraphWire,
  type RunOverlay,
  type NodeRunState,
  type Sop,
  type SopStep,
  type StepFailure,
} from '@/lib/sops';

function blankStep(number: number): SopStep {
  return {
    number,
    title: '',
    body: '',
    kind: 'execute',
    requires_confirmation: false,
    suggested_tools: [],
  };
}

function blankSop(name: string): Sop {
  return {
    name,
    description: '',
    version: '1.0.0',
    priority: 'normal',
    execution_mode: 'supervised',
    triggers: [{ type: 'manual' }],
    steps: [blankStep(1)],
    cooldown_secs: 0,
    max_concurrent: 1,
    deterministic: false,
  };
}

function nodeStateTone(state: NodeRunState | undefined): string {
  switch (state) {
    case 'active':
      return 'border-pc-accent ring-2 ring-pc-accent animate-pulse';
    case 'completed':
      return 'border-emerald-500';
    case 'failed':
      return 'border-rose-500';
    case 'skipped':
      return 'border-amber-500 opacity-70';
    default:
      return 'border-pc-border';
  }
}

function nodeStateBadgeTone(state: NodeRunState): 'ok' | 'error' | 'warn' | 'neutral' {
  switch (state) {
    case 'completed':
      return 'ok';
    case 'failed':
      return 'error';
    case 'skipped':
      return 'warn';
    default:
      return 'neutral';
  }
}

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

function NodeCard({ node, state }: { node: GraphNode; state?: NodeRunState }) {
  return (
    <div
      className={`w-full max-w-xl rounded-[var(--radius-lg)] border bg-pc-surface shadow-sm ${nodeStateTone(state)}`}
    >
      <div className="flex items-center gap-2 border-b border-pc-border px-3 py-2">
        <span className="inline-flex h-6 w-6 items-center justify-center rounded bg-pc-accent-light text-xs font-semibold text-pc-accent">
          {node.step}
        </span>
        <span className="font-medium text-pc-text">{node.title}</span>
        {state ? (
          <span className="ml-auto">
            <Badge tone={nodeStateBadgeTone(state)}>{t(`sops.run_state.${state}`)}</Badge>
          </span>
        ) : null}
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

function GraphCanvas({ graph, overlay }: { graph: SopGraph; overlay?: RunOverlay | null }) {
  const ordered = useMemo(
    () => [...graph.nodes].sort((a, b) => a.step - b.step),
    [graph.nodes],
  );
  const stateByStep = useMemo(() => {
    const map = new Map<number, NodeRunState>();
    for (const n of overlay?.nodes ?? []) map.set(n.step, n.state);
    return map;
  }, [overlay]);
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
        const nextStep = ordered[idx + 1]?.step;
        const flowsToActive =
          outbound.some((w) => stateByStep.get(w.to_step) === 'active') ||
          (nextStep !== undefined && stateByStep.get(nextStep) === 'active');
        return (
          <div key={node.step} className="flex w-full flex-col items-center">
            <NodeCard node={node} state={stateByStep.get(node.step)} />
            {idx < ordered.length - 1 || outbound.length > 0 ? (
              <div className="flex flex-col items-center py-1">
                <ArrowDown
                  className={`h-4 w-4 ${flowsToActive ? 'animate-bounce text-pc-accent' : 'text-pc-text-muted'}`}
                  aria-hidden
                />
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

function SopFieldList({
  graph,
  overlay,
}: {
  graph: SopGraph;
  overlay?: RunOverlay | null;
}) {
  const stateByStep = new Map<number, NodeRunState>();
  for (const n of overlay?.nodes ?? []) stateByStep.set(n.step, n.state);
  return (
    <div className="divide-y divide-pc-border rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface text-sm">
      {graph.nodes.map((node) => {
        const state = stateByStep.get(node.step);
        return (
          <div key={node.step} className="flex items-start gap-3 px-3 py-2">
            <span className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded bg-pc-accent-light text-xs font-semibold text-pc-accent">
              {node.step}
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="font-medium text-pc-text">{node.title}</span>
                {state ? (
                  <Badge tone={nodeStateBadgeTone(state)}>{t(`sops.run_state.${state}`)}</Badge>
                ) : null}
              </div>
              <div className="mt-0.5 text-xs text-pc-text-muted">
                {t('sops.inputs')}:{' '}
                {node.inputs.length === 0
                  ? '—'
                  : node.inputs.map((p) => `${p.name}:${pinTypeLabel(p)}`).join(', ')}
                {'  ·  '}
                {t('sops.outputs')}:{' '}
                {node.outputs.length === 0
                  ? '—'
                  : node.outputs.map((p) => `${p.name}:${pinTypeLabel(p)}`).join(', ')}
              </div>
            </div>
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

function failureKind(f: StepFailure | undefined): 'fail' | 'retry' | 'goto' {
  if (f === undefined || f === 'fail') return 'fail';
  if ('retry' in f) return 'retry';
  return 'goto';
}

function StepEditor({
  step,
  index,
  count,
  selected,
  onSelect,
  onChange,
  onRemove,
  onMove,
}: {
  step: SopStep;
  index: number;
  count: number;
  selected: boolean;
  onSelect: () => void;
  onChange: (patch: Partial<SopStep>) => void;
  onRemove: () => void;
  onMove: (dir: -1 | 1) => void;
}) {
  const rowRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (selected) rowRef.current?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }, [selected]);
  const routing = step.routing ?? {};
  const fkind = failureKind(step.on_failure);
  const setFailure = (kind: 'fail' | 'retry' | 'goto') => {
    if (kind === 'fail') onChange({ on_failure: 'fail' });
    else if (kind === 'retry') onChange({ on_failure: { retry: { max: 1 } } });
    else onChange({ on_failure: { goto: { step: 1 } } });
  };
  const setRouting = (patch: Partial<typeof routing>) =>
    onChange({ routing: { ...routing, ...patch } });
  return (
    <div
      ref={rowRef}
      onFocusCapture={onSelect}
      onClick={onSelect}
      className={`rounded-[var(--radius-lg)] border bg-pc-surface p-3 ${
        selected ? 'border-pc-accent ring-1 ring-pc-accent' : 'border-pc-border'
      }`}
    >
      <div className="mb-2 flex items-center gap-2">
        <span className="inline-flex h-6 w-6 items-center justify-center rounded bg-pc-accent-light text-xs font-semibold text-pc-accent">
          {step.number}
        </span>
        <input
          type="text"
          value={step.title}
          onChange={(e) => onChange({ title: e.target.value })}
          placeholder={t('sops.step_title_placeholder')}
          className="flex-1 rounded border border-pc-border bg-pc-surface px-2 py-1 text-sm text-pc-text"
        />
        <select
          value={step.kind ?? 'execute'}
          onChange={(e) => onChange({ kind: e.target.value as SopStep['kind'] })}
          className="rounded border border-pc-border bg-pc-surface px-1.5 py-1 text-xs text-pc-text"
          aria-label={t('sops.step_kind')}
        >
          <option value="execute">{t('sops.kind_execute')}</option>
          <option value="checkpoint">{t('sops.kind_checkpoint')}</option>
        </select>
        <button
          type="button"
          onClick={() => onMove(-1)}
          disabled={index === 0}
          className="rounded px-1.5 py-1 text-pc-text-muted hover:bg-pc-elevated disabled:opacity-30"
          aria-label={t('sops.move_up')}
        >
          ↑
        </button>
        <button
          type="button"
          onClick={() => onMove(1)}
          disabled={index === count - 1}
          className="rounded px-1.5 py-1 text-pc-text-muted hover:bg-pc-elevated disabled:opacity-30"
          aria-label={t('sops.move_down')}
        >
          ↓
        </button>
        <button
          type="button"
          onClick={onRemove}
          className="rounded px-1.5 py-1 text-rose-500 hover:bg-pc-elevated"
          aria-label={t('sops.remove_step')}
        >
          <Trash2 className="h-4 w-4" aria-hidden />
        </button>
      </div>
      <textarea
        value={step.body}
        onChange={(e) => onChange({ body: e.target.value })}
        placeholder={t('sops.step_body_placeholder')}
        rows={2}
        className="mb-2 w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-sm text-pc-text"
      />
      <div className="mb-2 flex items-center gap-3 text-xs">
        <input
          type="text"
          value={(step.suggested_tools ?? []).join(', ')}
          onChange={(e) =>
            onChange({
              suggested_tools: e.target.value
                .split(',')
                .map((s) => s.trim())
                .filter(Boolean),
            })
          }
          placeholder={t('sops.step_tools_placeholder')}
          className="flex-1 rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
        />
        <label className="flex items-center gap-1 text-pc-text-muted">
          <input
            type="checkbox"
            checked={step.requires_confirmation ?? false}
            onChange={(e) => onChange({ requires_confirmation: e.target.checked })}
          />
          {t('sops.requires_confirmation')}
        </label>
      </div>
      <div className="grid grid-cols-3 gap-2 border-t border-pc-border pt-2 text-xs">
        <label className="block">
          <span className="mb-1 block text-pc-text-muted">{t('sops.routing_depends_on')}</span>
          <input
            type="text"
            value={(routing.depends_on ?? []).join(', ')}
            onChange={(e) =>
              setRouting({
                depends_on: e.target.value
                  .split(',')
                  .map((s) => parseInt(s.trim(), 10))
                  .filter((n) => Number.isFinite(n)),
              })
            }
            placeholder="2, 3"
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          />
        </label>
        <label className="block">
          <span className="mb-1 block text-pc-text-muted">{t('sops.routing_next')}</span>
          <input
            type="number"
            value={routing.next ?? ''}
            onChange={(e) =>
              setRouting({ next: e.target.value ? parseInt(e.target.value, 10) : undefined })
            }
            placeholder="→"
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          />
        </label>
        <label className="block">
          <span className="mb-1 block text-pc-text-muted">{t('sops.routing_when')}</span>
          <input
            type="text"
            value={routing.when ?? ''}
            onChange={(e) => setRouting({ when: e.target.value || undefined })}
            placeholder="$.value > 85"
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          />
        </label>
        <label className="block">
          <span className="mb-1 block text-pc-text-muted">{t('sops.on_failure')}</span>
          <select
            value={fkind}
            onChange={(e) => setFailure(e.target.value as 'fail' | 'retry' | 'goto')}
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          >
            <option value="fail">{t('sops.failure_fail')}</option>
            <option value="retry">{t('sops.failure_retry')}</option>
            <option value="goto">{t('sops.failure_goto')}</option>
          </select>
        </label>
        {fkind === 'retry' && step.on_failure && typeof step.on_failure === 'object' && 'retry' in step.on_failure ? (
          <label className="block">
            <span className="mb-1 block text-pc-text-muted">{t('sops.failure_max')}</span>
            <input
              type="number"
              value={step.on_failure.retry.max}
              onChange={(e) =>
                onChange({ on_failure: { retry: { max: parseInt(e.target.value, 10) || 1 } } })
              }
              className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
            />
          </label>
        ) : null}
        {fkind === 'goto' && step.on_failure && typeof step.on_failure === 'object' && 'goto' in step.on_failure ? (
          <label className="block">
            <span className="mb-1 block text-pc-text-muted">{t('sops.failure_goto_step')}</span>
            <input
              type="number"
              value={step.on_failure.goto.step}
              onChange={(e) =>
                onChange({ on_failure: { goto: { step: parseInt(e.target.value, 10) || 1 } } })
              }
              className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
            />
          </label>
        ) : null}
      </div>
      <div className="mt-2 rounded border border-pc-border p-2">
        <div className="mb-1 flex items-center justify-between">
          <span className="text-xs font-medium text-pc-text">{t('sops.switch_ports')}</span>
          <button
            type="button"
            onClick={() =>
              setRouting({
                switch: [...(routing.switch ?? []), { name: `port ${(routing.switch?.length ?? 0) + 1}`, when: undefined, goto: undefined }],
              })
            }
            className="rounded border border-pc-border px-2 py-0.5 text-xs text-pc-text hover:bg-pc-elevated"
          >
            <Plus className="mr-1 inline h-3 w-3" aria-hidden />
            {t('sops.add_port')}
          </button>
        </div>
        {(routing.switch ?? []).length === 0 ? (
          <div className="text-xs text-pc-text-faint">{t('sops.no_ports')}</div>
        ) : (
          (routing.switch ?? []).map((rule, ri) => {
            const setRule = (patch: Partial<typeof rule>) => {
              const rules = [...(routing.switch ?? [])];
              rules[ri] = { ...rules[ri]!, ...patch };
              setRouting({ switch: rules });
            };
            return (
              <div key={ri} className="mb-1 grid grid-cols-[1fr_1.4fr_4rem_1.5rem] items-center gap-1">
                <input
                  type="text"
                  value={rule.name}
                  onChange={(e) => setRule({ name: e.target.value })}
                  placeholder={t('sops.port_name')}
                  className="rounded border border-pc-border bg-pc-surface px-1.5 py-0.5 text-xs text-pc-text"
                />
                <input
                  type="text"
                  value={rule.when ?? ''}
                  onChange={(e) => setRule({ when: e.target.value || undefined })}
                  placeholder={t('sops.port_when')}
                  className="rounded border border-pc-border bg-pc-surface px-1.5 py-0.5 text-xs text-pc-text"
                />
                <input
                  type="number"
                  value={rule.goto ?? ''}
                  onChange={(e) => setRule({ goto: e.target.value ? parseInt(e.target.value, 10) : undefined })}
                  placeholder="→"
                  className="rounded border border-pc-border bg-pc-surface px-1.5 py-0.5 text-xs text-pc-text"
                />
                <button
                  type="button"
                  onClick={() => setRouting({ switch: (routing.switch ?? []).filter((_, j) => j !== ri) })}
                  className="text-rose-500"
                  aria-label={t('sops.remove_port')}
                >
                  <Trash2 className="h-3.5 w-3.5" aria-hidden />
                </button>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}

function SopEditor({
  draft,
  saving,
  saveError,
  selectedStep,
  onSelectStep,
  onField,
  onStep,
  onAddStep,
  onRemoveStep,
  onMoveStep,
  onSave,
  onCancel,
}: {
  draft: Sop;
  saving: boolean;
  saveError: string | null;
  selectedStep: number | null;
  onSelectStep: (n: number) => void;
  onField: (patch: Partial<Sop>) => void;
  onStep: (i: number, patch: Partial<SopStep>) => void;
  onAddStep: () => void;
  onRemoveStep: (i: number) => void;
  onMoveStep: (i: number, dir: -1 | 1) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  return (
    <Card className="space-y-3">
      <div className="flex items-center justify-between">
        <div className="font-medium text-pc-text">{t('sops.editor_title')}</div>
        <div className="flex gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="inline-flex items-center gap-1 rounded border border-pc-border px-2 py-1 text-sm text-pc-text hover:bg-pc-elevated"
          >
            <X className="h-4 w-4" aria-hidden /> {t('sops.cancel')}
          </button>
          <button
            type="button"
            onClick={onSave}
            disabled={saving}
            className="inline-flex items-center gap-1 rounded bg-pc-accent px-2 py-1 text-sm text-white disabled:opacity-50"
          >
            {saving ? (
              <Loader2 className="h-4 w-4 animate-spin" aria-hidden />
            ) : (
              <Save className="h-4 w-4" aria-hidden />
            )}
            {t('sops.save')}
          </button>
        </div>
      </div>
      {saveError ? <div className="text-sm text-rose-500">{saveError}</div> : null}
      <div className="grid grid-cols-2 gap-3">
        <label className="text-sm">
          <span className="mb-1 block text-pc-text-muted">{t('sops.field_name')}</span>
          <input
            type="text"
            value={draft.name}
            onChange={(e) => onField({ name: e.target.value })}
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          />
        </label>
        <label className="text-sm">
          <span className="mb-1 block text-pc-text-muted">{t('sops.field_version')}</span>
          <input
            type="text"
            value={draft.version}
            onChange={(e) => onField({ version: e.target.value })}
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          />
        </label>
      </div>
      <label className="block text-sm">
        <span className="mb-1 block text-pc-text-muted">{t('sops.field_description')}</span>
        <input
          type="text"
          value={draft.description}
          onChange={(e) => onField({ description: e.target.value })}
          className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
        />
      </label>
      <div className="grid grid-cols-2 gap-3">
        <label className="text-sm">
          <span className="mb-1 block text-pc-text-muted">{t('sops.field_priority')}</span>
          <select
            value={draft.priority}
            onChange={(e) => onField({ priority: e.target.value as Sop['priority'] })}
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          >
            <option value="critical">critical</option>
            <option value="high">high</option>
            <option value="normal">normal</option>
            <option value="low">low</option>
          </select>
        </label>
        <label className="text-sm">
          <span className="mb-1 block text-pc-text-muted">{t('sops.field_execution_mode')}</span>
          <select
            value={draft.execution_mode}
            onChange={(e) => onField({ execution_mode: e.target.value as Sop['execution_mode'] })}
            className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text"
          >
            <option value="auto">auto</option>
            <option value="supervised">supervised</option>
            <option value="step_by_step">step_by_step</option>
            <option value="priority_based">priority_based</option>
            <option value="deterministic">deterministic</option>
          </select>
        </label>
      </div>
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <span className="text-sm font-medium text-pc-text">{t('sops.steps')}</span>
          <button
            type="button"
            onClick={onAddStep}
            className="inline-flex items-center gap-1 rounded border border-pc-border px-2 py-1 text-xs text-pc-text hover:bg-pc-elevated"
          >
            <Plus className="h-3.5 w-3.5" aria-hidden /> {t('sops.add_step')}
          </button>
        </div>
        {draft.steps.map((s, i) => (
          <StepEditor
            key={i}
            step={s}
            index={i}
            count={draft.steps.length}
            selected={selectedStep === s.number}
            onSelect={() => onSelectStep(s.number)}
            onChange={(patch) => onStep(i, patch)}
            onRemove={() => onRemoveStep(i)}
            onMove={(dir) => onMoveStep(i, dir)}
          />
        ))}
      </div>
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
  const [runId, setRunId] = useState('');
  const [overlay, setOverlay] = useState<RunOverlay | null>(null);
  const [overlayError, setOverlayError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Sop | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [layer, setLayer] = useState<'visual' | 'fields'>('visual');
  const [selectedStep, setSelectedStep] = useState<number | null>(null);

  const onConnect = useCallback(
    (from: number, to: number, kind: WireRole, portIndex?: number) => {
      setDraft((d) => {
        if (!d) return d;
        wireDraft(d, { op: 'connect', from, to, role: kind, port: portIndex })
          .then((res) => setDraft(res.sop))
          .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
        return d;
      });
    },
    [],
  );

  const onDisconnect = useCallback(
    (from: number, to: number, kind: WireRole, portIndex?: number) => {
      setDraft((d) => {
        if (!d) return d;
        wireDraft(d, { op: 'disconnect', from, to, role: kind, port: portIndex })
          .then((res) => setDraft(res.sop))
          .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
        return d;
      });
    },
    [],
  );

  const refreshList = useCallback((selectName?: string) => {
    return listSops()
      .then((list) => {
        setSops(list);
        if (selectName) setSelected(selectName);
        else if (list.length > 0 && !list.some((s) => s.name === selectName)) {
          setSelected(list[0]?.name ?? '');
        }
        return list;
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
        return [];
      });
  }, []);

  const renumber = (steps: SopStep[]): SopStep[] =>
    steps.map((s, i) => ({ ...s, number: i + 1 }));

  const startNew = useCallback(() => {
    setSaveError(null);
    setDraft(blankSop(''));
  }, []);

  const startEdit = useCallback(() => {
    if (!selected) return;
    setSaveError(null);
    getSop(selected)
      .then((full) => setDraft(full))
      .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
  }, [selected]);

  const onSaveDraft = useCallback(() => {
    if (!draft) return;
    setSaving(true);
    setSaveError(null);
    const isNew = !sops.some((s) => s.name === draft.name);
    const body = { ...draft, steps: renumber(draft.steps) };
    const op = isNew ? createSop(body) : saveSop(body);
    op.then(() => {
      setSaving(false);
      setDraft(null);
      return refreshList(body.name);
    }).catch((e: unknown) => {
      setSaving(false);
      setSaveError(e instanceof Error ? e.message : String(e));
    });
  }, [draft, sops, refreshList]);

  const onDeleteSelected = useCallback(() => {
    if (!selected) return;
    deleteSop(selected)
      .then(() => refreshList())
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  }, [selected, refreshList]);

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

  useEffect(() => {
    setRunId('');
    setOverlay(null);
    setOverlayError(null);
  }, [selected]);

  useEffect(() => {
    if (!selected || !runId) {
      setOverlay(null);
      return;
    }
    let active = true;
    const poll = () => {
      getRunOverlay(selected, runId)
        .then((o) => {
          if (!active) return;
          setOverlay(o);
          setOverlayError(null);
        })
        .catch((e: unknown) => {
          if (!active) return;
          setOverlay(null);
          setOverlayError(e instanceof Error ? e.message : String(e));
        });
    };
    poll();
    const id = window.setInterval(poll, 2000);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [selected, runId]);

  const runStateByStep = useMemo(() => {
    const map = new Map<number, NodeRunState>();
    for (const n of overlay?.nodes ?? []) map.set(n.step, n.state);
    return map;
  }, [overlay]);

  const editorHandlers = draft
    ? {
        onField: (patch: Partial<Sop>) => setDraft((d) => (d ? { ...d, ...patch } : d)),
        onStep: (i: number, patch: Partial<SopStep>) =>
          setDraft((d) =>
            d ? { ...d, steps: d.steps.map((s, j) => (j === i ? { ...s, ...patch } : s)) } : d,
          ),
        onAddStep: () =>
          setDraft((d) =>
            d ? { ...d, steps: [...d.steps, blankStep(d.steps.length + 1)] } : d,
          ),
        onRemoveStep: (i: number) =>
          setDraft((d) => (d ? { ...d, steps: d.steps.filter((_, j) => j !== i) } : d)),
        onMoveStep: (i: number, dir: -1 | 1) =>
          setDraft((d) => {
            if (!d) return d;
            const j = i + dir;
            if (j < 0 || j >= d.steps.length) return d;
            const steps = [...d.steps];
            [steps[i], steps[j]] = [steps[j]!, steps[i]!];
            return { ...d, steps };
          }),
      }
    : null;

  return (
    <div className="space-y-4">
      <PageHeader
        title={t('sops.title')}
        description={t('sops.subtitle')}
        actions={
          !draft ? (
            <button
              type="button"
              onClick={startNew}
              className="inline-flex items-center gap-1 rounded bg-pc-accent px-3 py-1.5 text-sm text-white"
            >
              <Plus className="h-4 w-4" aria-hidden /> {t('sops.new')}
            </button>
          ) : null
        }
      />
      {error ? (
        <Card>
          <div className="text-rose-500">{error}</div>
        </Card>
      ) : null}
      {draft && editorHandlers ? (
        <div className="space-y-4">
          <SopCanvas
            draft={draft}
            selectedStep={selectedStep}
            runStateByStep={runStateByStep}
            onSelectStep={setSelectedStep}
            onAddStep={editorHandlers.onAddStep}
            onConnect={onConnect}
            onDisconnect={onDisconnect}
          />
          <SopEditor
            draft={draft}
            saving={saving}
            saveError={saveError}
            selectedStep={selectedStep}
            onSelectStep={setSelectedStep}
            onField={editorHandlers.onField}
            onStep={editorHandlers.onStep}
            onAddStep={editorHandlers.onAddStep}
            onRemoveStep={editorHandlers.onRemoveStep}
            onMoveStep={editorHandlers.onMoveStep}
            onSave={onSaveDraft}
            onCancel={() => setDraft(null)}
          />
        </div>
      ) : loading ? (
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
              <div className="ml-auto flex gap-2">
                <button
                  type="button"
                  onClick={() => setLayer((l) => (l === 'visual' ? 'fields' : 'visual'))}
                  disabled={!graph}
                  className="rounded border border-pc-border px-2 py-1 text-sm text-pc-text hover:bg-pc-elevated disabled:opacity-40"
                >
                  {layer === 'visual' ? t('sops.layer_fields') : t('sops.layer_visual')}
                </button>
                <button
                  type="button"
                  onClick={startEdit}
                  disabled={!graph}
                  className="rounded border border-pc-border px-2 py-1 text-sm text-pc-text hover:bg-pc-elevated disabled:opacity-40"
                >
                  {t('sops.edit')}
                </button>
                <button
                  type="button"
                  onClick={onDeleteSelected}
                  className="inline-flex items-center gap-1 rounded border border-pc-border px-2 py-1 text-sm text-rose-500 hover:bg-pc-elevated"
                >
                  <Trash2 className="h-4 w-4" aria-hidden /> {t('sops.delete')}
                </button>
              </div>
            </div>
            <div className="mb-3 flex items-center gap-2">
              <input
                type="text"
                value={runId}
                onChange={(e) => setRunId(e.target.value.trim())}
                placeholder={t('sops.run_id_placeholder')}
                className="w-64 rounded border border-pc-border bg-pc-surface px-2 py-1 text-sm text-pc-text"
              />
              {overlay ? (
                <Badge tone={overlay.status === 'failed' ? 'error' : 'ok'}>
                  {t(`sops.run_status.${overlay.status}`)} · {overlay.current_step}/
                  {overlay.total_steps}
                </Badge>
              ) : null}
              {overlayError ? <span className="text-xs text-rose-500">{overlayError}</span> : null}
            </div>
            {graphLoading ? (
              <Loader2 className="h-5 w-5 animate-spin text-pc-text-muted" aria-hidden />
            ) : graph ? (
              <>
                {layer === 'visual' ? (
                  <GraphCanvas graph={graph} overlay={overlay} />
                ) : (
                  <SopFieldList graph={graph} overlay={overlay} />
                )}
                <DiagnosticsPanel graph={graph} />
              </>
            ) : null}
          </div>
        </div>
      )}
    </div>
  );
}
