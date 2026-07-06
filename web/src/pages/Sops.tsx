import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { AlertTriangle, XCircle, Loader2, Plus, Save, Trash2, X } from 'lucide-react';
import { Link } from 'react-router-dom';
import { Badge, Card, PageHeader, HelpTip } from '@/components/ui';
import SopCanvas from './SopCanvas';
import MarkdownEditor from '@/components/MarkdownEditor';
import ToolPicker from '@/components/ToolPicker';
import { PlannedCallsEditor, CapturedCallList } from '@/components/SopCalls';
import { t } from '@/lib/i18n';
import { loadAgentPickerSummaries } from '@/lib/agents';
import {
  listSops,
  getSopGraph,
  getRunOverlay,
  getSop,
  saveSop,
  deleteSop,
  wireDraft,
  graphDraft,
  triggerSources,
  sopFieldHelp,
  overlayStateByStep,
  overlayCallsByStep,
  runStateTone,
  parseCondition,
  buildCondition,
  type RunStateTone,
  type WireRole,
  type SopSummary,
  type SopGraph,
  type GraphPin,
  type RunOverlay,
  type NodeRunState,
  type Sop,
  type SopStep,
  type SopTrigger,
  type StepFailure,
  type StepToolCall,
  type TriggerSourceRegistry,
  type BoundTriggerSource,
  type TriggerField,
  type PayloadContract,
  type ConditionOpSpec,
  type ConditionField,
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

const DRAFT_STORAGE_KEY = 'zeroclaw_sop_draft';
const DRAFT_EDITING_NAME_KEY = 'zeroclaw_sop_editing_name';

function setArgAtPath(
  root: Record<string, unknown>,
  segments: string[],
  value: string | null,
): Record<string, unknown> {
  const head = segments[0];
  if (head === undefined) return root;
  const rest = segments.slice(1);
  const next = { ...root };
  if (rest.length === 0) {
    if (value === null) delete next[head];
    else next[head] = value;
    return next;
  }
  const child = typeof next[head] === 'object' && next[head] !== null ? (next[head] as Record<string, unknown>) : {};
  next[head] = setArgAtPath(child, rest, value);
  return next;
}

function writeStepBinding(sop: Sop, toStep: number, toPin: string, value: string | null): Sop {
  const segments = toPin.split('.');
  if (segments[0] !== 'calls') return sop;
  const callIdx = Number(segments[1]);
  const argSegments = segments.slice(2);
  if (Number.isNaN(callIdx) || argSegments.length === 0) return sop;
  return {
    ...sop,
    steps: sop.steps.map((step) => {
      if (step.number !== toStep || !step.calls) return step;
      return {
        ...step,
        calls: step.calls.map((call, idx) => {
          if (idx !== callIdx) return call;
          const args = (call.args ?? {}) as Record<string, unknown>;
          return { ...call, args: setArgAtPath(args, argSegments, value) };
        }),
      };
    }),
  };
}

function loadStoredDraft(): Sop | null {
  try {
    const raw = sessionStorage.getItem(DRAFT_STORAGE_KEY);
    return raw ? (JSON.parse(raw) as Sop) : null;
  } catch {
    return null;
  }
}

function storeDraft(draft: Sop | null): void {
  try {
    if (draft) sessionStorage.setItem(DRAFT_STORAGE_KEY, JSON.stringify(draft));
    else sessionStorage.removeItem(DRAFT_STORAGE_KEY);
  } catch {
    // Storage is best-effort; a failure only loses cross-navigation recovery.
  }
}

function loadStoredEditingName(): string | null {
  try {
    return sessionStorage.getItem(DRAFT_EDITING_NAME_KEY);
  } catch {
    return null;
  }
}

function storeEditingName(name: string | null): void {
  try {
    if (name !== null) sessionStorage.setItem(DRAFT_EDITING_NAME_KEY, name);
    else sessionStorage.removeItem(DRAFT_EDITING_NAME_KEY);
  } catch {
    // Best-effort; a failure only degrades a post-reload rename into a fork.
  }
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

const BADGE_TONE: Record<RunStateTone, 'ok' | 'error' | 'warn' | 'neutral'> = {
  accent: 'neutral',
  success: 'ok',
  error: 'error',
  warning: 'warn',
  neutral: 'neutral',
};

function nodeStateBadgeTone(state: NodeRunState): 'ok' | 'error' | 'warn' | 'neutral' {
  return BADGE_TONE[runStateTone(state)];
}

function pinTypeLabel(pin: GraphPin): string {
  if (pin.class === 'flow') return 'flow';
  return pin.data_type ?? 'any';
}

function SopFieldList({
  graph,
  overlay,
}: {
  graph: SopGraph;
  overlay?: RunOverlay | null;
}) {
  const stateByStep = overlayStateByStep(overlay);
  const callsByStep = overlayCallsByStep(overlay);
  return (
    <div className="divide-y divide-pc-border rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface text-sm">
      {graph.nodes.map((node) => {
        const state = stateByStep.get(node.step);
        const calls = callsByStep.get(node.step);
        return (
          <div key={node.step} className="flex items-start gap-3 px-3 py-2">
            <span className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded bg-pc-accent text-xs font-semibold text-[#0b1220]">
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
              {calls ? (
                <div className="mt-2">
                  <div className="mb-1 text-xs font-medium text-pc-text">
                    {t('sops.captured_calls')}
                  </div>
                  <CapturedCallList calls={calls} />
                </div>
              ) : null}
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
              <XCircle className="mt-0.5 h-4 w-4 shrink-0 text-status-error" aria-hidden />
            ) : (
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-status-warning" aria-hidden />
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

const INPUT_CLS = 'w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text';

function StepBodyEditor({
  value,
  onChange,
}: {
  value: string;
  onChange: (next: string) => void;
}) {
  const [focused, setFocused] = useState(false);
  return (
    <div>
      <span className="mb-1 block text-pc-text-muted text-sm">
        <HelpTip text={sopFieldHelp('SopStep', 'body')}>{t('sops.step_body_label')}</HelpTip>
      </span>
      <MarkdownEditor
        value={value}
        onChange={onChange}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        height={focused ? '20rem' : '4rem'}
        lineNumbers={focused}
        placeholder={t('sops.step_body_placeholder')}
      />
    </div>
  );
}

function Field({
  label,
  hint,
  help,
  children,
}: {
  label: string;
  hint?: string | null;
  help?: string | null;
  children: ReactNode;
}) {
  return (
    <label className="block text-sm">
      <span className="mb-1 block text-pc-text-muted">
        {help ? <HelpTip text={help}>{label}</HelpTip> : label}
      </span>
      {children}
      {hint ? <p className="mt-1 text-xs text-pc-text-faint">{hint}</p> : null}
    </label>
  );
}

function TextField({
  label,
  value,
  onChange,
  placeholder,
  help,
}: {
  label: string;
  value: string;
  onChange: (next: string) => void;
  placeholder?: string;
  help?: string | null;
}) {
  return (
    <Field label={label} help={help}>
      <input
        type="text"
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        className={INPUT_CLS}
      />
    </Field>
  );
}

function SelectField({
  label,
  value,
  onChange,
  options,
  disabled,
  children,
  help,
}: {
  label: string;
  value: string;
  onChange: (next: string) => void;
  options?: string[];
  disabled?: boolean;
  children?: ReactNode;
  help?: string | null;
}) {
  return (
    <Field label={label} help={help}>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        className={INPUT_CLS}
      >
        {children}
        {(options ?? []).map((opt) => (
          <option key={opt} value={opt}>
            {opt}
          </option>
        ))}
      </select>
    </Field>
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
  capturedCalls,
  onChange,
  onRemove,
  onMove,
  agentAliases,
  parentAgent,
}: {
  step: SopStep;
  index: number;
  count: number;
  capturedCalls?: StepToolCall[];
  onChange: (patch: Partial<SopStep>) => void;
  onRemove: () => void;
  onMove: (dir: -1 | 1) => void;
  agentAliases: string[];
  parentAgent?: string | null;
}) {
  const rowRef = useRef<HTMLDivElement | null>(null);
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
      className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-3"
    >
      <div className="mb-2 flex items-center gap-2">
        <HelpTip text={sopFieldHelp('SopStep', 'number')}>
          <span className="inline-flex h-6 w-6 items-center justify-center rounded bg-pc-accent text-xs font-semibold text-[#0b1220]">
            {step.number}
          </span>
        </HelpTip>
        <input
          type="text"
          value={step.title}
          onChange={(e) => onChange({ title: e.target.value })}
          placeholder={t('sops.step_title_placeholder')}
          title={sopFieldHelp('SopStep', 'title') ?? undefined}
          className="flex-1 rounded border border-pc-border bg-pc-surface px-2 py-1 text-sm text-pc-text"
        />
        <select
          value={step.kind ?? 'execute'}
          onChange={(e) => onChange({ kind: e.target.value as SopStep['kind'] })}
          className="rounded border border-pc-border bg-pc-surface px-1.5 py-1 text-xs text-pc-text"
          aria-label={t('sops.step_kind')}
          title={sopFieldHelp('SopStep', 'kind') ?? undefined}
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
          className="rounded px-1.5 py-1 text-status-error hover:bg-pc-elevated"
          aria-label={t('sops.remove_step')}
        >
          <Trash2 className="h-4 w-4" aria-hidden />
        </button>
      </div>
      <div className="mb-2">
        <StepBodyEditor
          value={step.body}
          onChange={(next) => onChange({ body: next })}
        />
      </div>
      <div className="mb-2">
        <span className="mb-1 block text-pc-text-muted text-sm">
          <HelpTip text={sopFieldHelp('SopStep', 'agent')}>{t('sops.step_agent_label')}</HelpTip>
        </span>
        <select
          value={step.agent ?? ''}
          onChange={(e) => onChange({ agent: e.target.value === '' ? null : e.target.value })}
          className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-sm text-pc-text"
        >
          <option value="">
            {t('sops.step_agent_inherit')}
            {parentAgent ? ` (${parentAgent})` : ''}
          </option>
          {agentAliases.map((alias) => (
            <option key={alias} value={alias}>
              {alias}
            </option>
          ))}
        </select>
      </div>
      <div className="mb-2 space-y-2 text-xs">
        <div>
          <span className="mb-1 block text-pc-text-muted">
            <HelpTip text={sopFieldHelp('SopStep', 'suggested_tools')}>
              {t('sops.step_tools_label')}
            </HelpTip>
          </span>
          <ToolPicker
            value={step.suggested_tools ?? []}
            onChange={(next) => onChange({ suggested_tools: next })}
          />
        </div>
        <label className="flex items-center gap-1 text-pc-text-muted">
          <input
            type="checkbox"
            checked={step.requires_confirmation ?? false}
            onChange={(e) => onChange({ requires_confirmation: e.target.checked })}
          />
          <HelpTip text={sopFieldHelp('SopStep', 'requires_confirmation')}>
            {t('sops.requires_confirmation')}
          </HelpTip>
        </label>
      </div>
      <div className="grid grid-cols-3 gap-2 border-t border-pc-border pt-2 text-xs">
        <TextField
          label={t('sops.routing_depends_on')}
          value={(routing.depends_on ?? []).join(', ')}
          placeholder="2, 3"
          help={sopFieldHelp('StepRouting', 'depends_on')}
          onChange={(v) =>
            setRouting({
              depends_on: v
                .split(',')
                .map((s) => parseInt(s.trim(), 10))
                .filter((n) => Number.isFinite(n)),
            })
          }
        />
        <Field label={t('sops.routing_next')} help={sopFieldHelp('StepRouting', 'next')}>
          <input
            type="number"
            value={routing.next ?? ''}
            onChange={(e) =>
              setRouting({ next: e.target.value ? parseInt(e.target.value, 10) : undefined })
            }
            placeholder="→"
            className={INPUT_CLS}
          />
        </Field>
        <TextField
          label={t('sops.routing_when')}
          value={routing.when ?? ''}
          placeholder="$.value > 85"
          help={sopFieldHelp('StepRouting', 'when')}
          onChange={(v) => setRouting({ when: v || undefined })}
        />
        <SelectField
          label={t('sops.on_failure')}
          value={fkind}
          help={sopFieldHelp('SopStep', 'on_failure')}
          onChange={(v) => setFailure(v as 'fail' | 'retry' | 'goto')}
        >
          <option value="fail">{t('sops.failure_fail')}</option>
          <option value="retry">{t('sops.failure_retry')}</option>
          <option value="goto">{t('sops.failure_goto')}</option>
        </SelectField>
        {fkind === 'retry' && step.on_failure && typeof step.on_failure === 'object' && 'retry' in step.on_failure ? (
          <Field label={t('sops.failure_max')}>
            <input
              type="number"
              value={step.on_failure.retry.max}
              onChange={(e) =>
                onChange({ on_failure: { retry: { max: parseInt(e.target.value, 10) || 1 } } })
              }
              className={INPUT_CLS}
            />
          </Field>
        ) : null}
        {fkind === 'goto' && step.on_failure && typeof step.on_failure === 'object' && 'goto' in step.on_failure ? (
          <Field label={t('sops.failure_goto_step')}>
            <input
              type="number"
              value={step.on_failure.goto.step}
              onChange={(e) =>
                onChange({ on_failure: { goto: { step: parseInt(e.target.value, 10) || 1 } } })
              }
              className={INPUT_CLS}
            />
          </Field>
        ) : null}
      </div>
      <div className="mt-2 rounded border border-pc-border p-2">
        <div className="mb-1 flex items-center justify-between">
          <span className="text-xs font-medium text-pc-text">
            <HelpTip text={sopFieldHelp('StepRouting', 'switch')}>{t('sops.switch_ports')}</HelpTip>
          </span>
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
                  className="text-status-error"
                  aria-label={t('sops.remove_port')}
                >
                  <Trash2 className="h-3.5 w-3.5" aria-hidden />
                </button>
              </div>
            );
          })
        )}
      </div>
      <div className="mt-2">
        <PlannedCallsEditor
          calls={step.calls ?? []}
          captured={capturedCalls}
          onChange={(next) => onChange({ calls: next })}
        />
      </div>
    </div>
  );
}

const CHANNEL_SOURCE = 'channel';
const MANUAL_SOURCE = 'manual';

function triggerSource(trigger: SopTrigger): string {
  return trigger.type === CHANNEL_SOURCE ? CHANNEL_SOURCE : trigger.type;
}

/// Blank value for a registry field, shaped by its declared kind. No field
/// names are consulted: the registry's `kind` is the single authority.
function blankFieldValue(field: TriggerField): unknown {
  switch (field.kind) {
    case 'list':
      return [];
    case 'expression':
      return null;
    default:
      return '';
  }
}

/// Build a fresh trigger for a chosen source. The registry supplies the field
/// set; each bound field starts at its kind's blank value and the channel
/// source starts on the first walked channel kind. No per-source field logic
/// is hardcoded.
function blankTrigger(
  source: string,
  registry: TriggerSourceRegistry | null,
): SopTrigger {
  if (source === CHANNEL_SOURCE) {
    const firstChannel = registry?.channels[0]?.channel ?? '';
    return { type: 'channel', channel: firstChannel, alias: null, condition: null };
  }
  if (source === MANUAL_SOURCE) return { type: 'manual' };
  const bound = registry?.bound.find((b) => b.source === source);
  const base: Record<string, unknown> = { type: source };
  for (const field of bound?.fields ?? []) {
    base[field.name] = blankFieldValue(field);
  }
  return base as unknown as SopTrigger;
}

/// i18n lookup with a fallback: `t()` returns the key itself when no
/// translation exists, so detect that and fall back instead of leaking keys.
function tOr(key: string, fallback: string | null): string | null {
  const value = t(key);
  return value === key ? fallback : value;
}

function triggerFieldLabel(field: string): string {
  return tOr(`sops.trigger_${field}`, field) ?? field;
}

function triggerFieldHint(field: string): string | null {
  return tOr(`sops.trigger_${field}_hint`, null);
}

function triggerFieldPlaceholder(field: string): string {
  return tOr(`sops.trigger_${field}_placeholder`, null) ?? '';
}

function TriggerFieldInput({
  field,
  value,
  onChange,
}: {
  field: TriggerField;
  value: unknown;
  onChange: (next: unknown) => void;
}) {
  const name = field.name;
  const options = field.options ?? [];
  const hint = triggerFieldHint(name);
  const help = sopFieldHelp('SopTrigger', name);

  if (options.length > 0) {
    if (field.multi) {
      const selected = new Set(Array.isArray(value) ? (value as string[]) : []);
      const toggle = (opt: string) => {
        const next = new Set(selected);
        if (next.has(opt)) next.delete(opt);
        else next.add(opt);
        onChange(options.filter((o) => next.has(o)));
      };
      return (
        <fieldset className="block text-sm">
          <legend className="mb-1 block text-pc-text-muted">
            {help ? (
              <HelpTip text={help}>{triggerFieldLabel(name)}</HelpTip>
            ) : (
              triggerFieldLabel(name)
            )}
          </legend>
          <div className="flex flex-wrap gap-2">
            {options.map((opt) => (
              <label
                key={opt}
                className="inline-flex items-center gap-1.5 rounded border border-pc-border px-2 py-1 text-xs text-pc-text"
              >
                <input
                  type="checkbox"
                  checked={selected.has(opt)}
                  onChange={() => toggle(opt)}
                />
                {opt}
              </label>
            ))}
          </div>
          {hint ? <p className="mt-1 text-xs text-pc-text-faint">{hint}</p> : null}
        </fieldset>
      );
    }
    const current = typeof value === 'string' ? value : '';
    return (
      <Field label={triggerFieldLabel(name)} hint={hint} help={help}>
        <select value={current} onChange={(e) => onChange(e.target.value)} className={INPUT_CLS}>
          {options.map((opt) => (
            <option key={opt} value={opt}>
              {opt}
            </option>
          ))}
        </select>
      </Field>
    );
  }

  const isList = field.kind === 'list';
  const isExpression = field.kind === 'expression';
  const text = isList
    ? Array.isArray(value)
      ? value.join(', ')
      : ''
    : typeof value === 'string'
      ? value
      : '';
  return (
    <Field label={triggerFieldLabel(name)} hint={hint} help={help}>
      <input
        type="text"
        value={text}
        placeholder={triggerFieldPlaceholder(name)}
        onChange={(e) => {
          const raw = e.target.value;
          if (isList) {
            onChange(
              raw
                .split(',')
                .map((s) => s.trim())
                .filter((s) => s.length > 0),
            );
          } else if (isExpression) {
            onChange(raw.length > 0 ? raw : null);
          } else {
            onChange(raw);
          }
        }}
        className={INPUT_CLS}
      />
    </Field>
  );
}

function conditionValueInputType(vt: ConditionField['value_type'] | undefined): string {
  if (vt === 'number') return 'number';
  if (vt === 'date_time') return 'datetime-local';
  return 'text';
}

/// Guided condition builder. Users pick a payload field (from the source's
/// walked contract), an operator (from the registry catalog), and a value;
/// the three assemble into the `$.path op value` string the engine evaluates.
/// No operator or path is typed blind. `open` payloads (mqtt, amqp, channel)
/// have no enumerated fields, so the path becomes a free input with an
/// advanced raw-string escape hatch; `direct` scalar payloads drop the path
/// entirely. Sources with no contract render nothing (condition unsupported).
function ConditionBuilder({
  contract,
  operators,
  value,
  onChange,
}: {
  contract: PayloadContract | null | undefined;
  operators: ConditionOpSpec[];
  value: string | null;
  onChange: (next: string | null) => void;
}) {
  const parsed = parseCondition(value, operators);
  const [raw, setRaw] = useState(false);
  if (!contract) return null;

  const fields = contract.fields ?? [];
  const isDirect = contract.direct === true;
  const isOpen = contract.open === true && fields.length === 0 && !isDirect;
  const selectedField = fields.find((f) => `${f.path}` === parsed.path);
  const valueType = selectedField?.value_type;

  const emit = (part: { path: string | null; op: string; value: string }) =>
    onChange(buildCondition(part));

  if (raw && isOpen) {
    return (
      <div className="space-y-1">
        <Field
          label={t('sops.trigger_condition')}
          hint={t('sops.condition_raw_hint')}
          help={sopFieldHelp('SopTrigger', 'condition')}
        >
          <input
            type="text"
            value={parsed.raw}
            placeholder={t('sops.trigger_condition_placeholder')}
            onChange={(e) => onChange(e.target.value.length > 0 ? e.target.value : null)}
            className={INPUT_CLS}
          />
        </Field>
        <button
          type="button"
          onClick={() => setRaw(false)}
          className="text-xs text-pc-text-muted underline hover:text-pc-accent"
        >
          {t('sops.condition_use_builder')}
        </button>
      </div>
    );
  }

  return (
    <fieldset className="space-y-2">
      <legend className="mb-1 block text-sm text-pc-text-muted">
        <HelpTip text={sopFieldHelp('SopTrigger', 'condition')}>
          {t('sops.trigger_condition')}
        </HelpTip>
      </legend>
      <div className="grid grid-cols-[1.4fr_auto_1.4fr] items-end gap-2">
        {isDirect ? (
          <div className="text-xs text-pc-text-faint">{t('sops.condition_direct_payload')}</div>
        ) : isOpen ? (
          <Field label={t('sops.condition_field')}>
            <input
              type="text"
              value={parsed.path ?? ''}
              placeholder="path.to.field"
              onChange={(e) =>
                emit({ path: e.target.value, op: parsed.op, value: parsed.value })
              }
              className={INPUT_CLS}
            />
          </Field>
        ) : (
          <Field label={t('sops.condition_field')}>
            <select
              value={parsed.path ?? ''}
              onChange={(e) => emit({ path: e.target.value, op: parsed.op, value: parsed.value })}
              className={INPUT_CLS}
            >
              <option value="">{t('sops.condition_pick_field')}</option>
              {fields.map((f) => (
                <option key={f.path} value={f.path}>
                  {f.label}
                </option>
              ))}
            </select>
          </Field>
        )}
        <Field label={t('sops.condition_operator')}>
          <select
            value={parsed.op}
            onChange={(e) =>
              emit({ path: parsed.path, op: e.target.value, value: parsed.value })
            }
            className={INPUT_CLS}
          >
            <option value="">{t('sops.condition_any')}</option>
            {operators.map((op) => (
              <option key={op.token} value={op.token}>
                {op.label} ({op.token})
              </option>
            ))}
          </select>
        </Field>
        {selectedField?.options && selectedField.options.length > 0 ? (
          <Field label={t('sops.condition_value')}>
            <select
              value={parsed.value}
              onChange={(e) =>
                emit({ path: parsed.path, op: parsed.op, value: e.target.value })
              }
              className={INPUT_CLS}
            >
              <option value="">{t('sops.condition_pick_value')}</option>
              {selectedField.options.map((opt) => (
                <option key={opt} value={opt}>
                  {opt}
                </option>
              ))}
            </select>
          </Field>
        ) : (
          <Field label={t('sops.condition_value')}>
            <input
              type={conditionValueInputType(valueType)}
              value={parsed.value}
              placeholder={t('sops.condition_value_placeholder')}
              onChange={(e) =>
                emit({ path: parsed.path, op: parsed.op, value: e.target.value })
              }
              className={INPUT_CLS}
            />
          </Field>
        )}
      </div>
      {isOpen ? (
        <button
          type="button"
          onClick={() => setRaw(true)}
          className="text-xs text-pc-text-muted underline hover:text-pc-accent"
        >
          {t('sops.condition_use_raw')}
        </button>
      ) : null}
    </fieldset>
  );
}

function ChannelTriggerFields({
  trigger,
  registry,
  onChange,
}: {
  trigger: Extract<SopTrigger, { type: 'channel' }>;
  registry: TriggerSourceRegistry | null;
  onChange: (patch: Partial<Extract<SopTrigger, { type: 'channel' }>>) => void;
}) {
  const channels = registry?.channels ?? [];
  const selected = channels.find((c) => c.channel === trigger.channel);
  return (
    <div className="space-y-2">
      <div className="grid grid-cols-2 gap-3">
        <SelectField
          label={t('sops.trigger_channel')}
          value={trigger.channel}
          onChange={(v) => onChange({ channel: v, alias: null })}
          options={channels.map((c) => c.channel)}
          help={sopFieldHelp('SopTrigger', 'channel')}
        />
        <SelectField
          label={t('sops.trigger_alias')}
          value={trigger.alias ?? ''}
          onChange={(v) => onChange({ alias: v.length > 0 ? v : null })}
          disabled={!selected?.configured}
          options={(selected?.aliases ?? []).map((a) => a.alias)}
          help={sopFieldHelp('SopTrigger', 'alias')}
        >
          <option value="">{t('sops.trigger_alias_any')}</option>
        </SelectField>
      </div>
      {selected && !selected.configured ? (
        <div className="flex items-center gap-2 text-xs text-status-warning">
          <AlertTriangle className="h-3.5 w-3.5" aria-hidden />
          <span>{t('sops.trigger_unconfigured')}</span>
          <Link
            to={selected.setup_path}
            className="underline hover:text-pc-accent"
          >
            {t('sops.trigger_setup_link')}
          </Link>
        </div>
      ) : null}
      <ConditionBuilder
        contract={selected?.condition}
        operators={registry?.operators ?? []}
        value={trigger.condition}
        onChange={(next) => onChange({ condition: next })}
      />
    </div>
  );
}

function TriggerEditor({
  trigger,
  index,
  selected,
  registry,
  onChange,
  onRemove,
}: {
  trigger: SopTrigger;
  index: number;
  selected: boolean;
  registry: TriggerSourceRegistry | null;
  onChange: (next: SopTrigger) => void;
  onRemove: () => void;
}) {
  const source = triggerSource(trigger);
  const bound = registry?.bound ?? [];
  const boundFields: TriggerField[] =
    source === CHANNEL_SOURCE || source === MANUAL_SOURCE
      ? []
      : (bound.find((b) => b.source === source)?.fields ?? []);
  const boundContract: PayloadContract | null =
    source === CHANNEL_SOURCE || source === MANUAL_SOURCE
      ? null
      : (bound.find((b) => b.source === source)?.condition ?? null);

  const sources: string[] = registry?.sources ?? [
    ...bound.map((b: BoundTriggerSource) => b.source),
    CHANNEL_SOURCE,
  ];

  return (
    <div
      className={`space-y-2 rounded border bg-pc-surface p-2 ${
        selected ? 'border-pc-accent' : 'border-pc-border'
      }`}
    >
      <div className="flex items-center justify-between gap-2">
        <div className="flex-1">
          <SelectField
            label={t('sops.trigger_source')}
            value={source}
            onChange={(v) => onChange(blankTrigger(v, registry))}
            options={sources}
          />
        </div>
        <button
          type="button"
          onClick={onRemove}
          className="mt-5 inline-flex items-center rounded border border-pc-border p-1 text-pc-text-muted hover:bg-pc-elevated"
          aria-label={t('sops.remove_trigger')}
        >
          <Trash2 className="h-3.5 w-3.5" aria-hidden />
        </button>
      </div>
      {trigger.type === CHANNEL_SOURCE ? (
        <ChannelTriggerFields
          trigger={trigger}
          registry={registry}
          onChange={(patch) => onChange({ ...trigger, ...patch })}
        />
      ) : source === MANUAL_SOURCE ? (
        <p className="text-xs text-pc-text-muted">{t('sops.trigger_manual_hint')}</p>
      ) : (
        <div className="space-y-2">
          {boundFields.map((field) => (
            <TriggerFieldInput
              key={field.name}
              field={field}
              value={(trigger as unknown as Record<string, unknown>)[field.name]}
              onChange={(next) =>
                onChange({
                  ...(trigger as unknown as Record<string, unknown>),
                  [field.name]: next,
                } as unknown as SopTrigger)
              }
            />
          ))}
          {boundContract ? (
            <ConditionBuilder
              contract={boundContract}
              operators={registry?.operators ?? []}
              value={
                ((trigger as unknown as Record<string, unknown>).condition as string | null) ??
                null
              }
              onChange={(next) =>
                onChange({
                  ...(trigger as unknown as Record<string, unknown>),
                  condition: next,
                } as unknown as SopTrigger)
              }
            />
          ) : null}
        </div>
      )}
      <span className="sr-only">{`trigger ${index + 1}`}</span>
    </div>
  );
}

function StepListRow({
  step,
  index,
  count,
  selected,
  onSelect,
  onMove,
  onRemove,
}: {
  step: SopStep;
  index: number;
  count: number;
  selected: boolean;
  onSelect: () => void;
  onMove: (dir: -1 | 1) => void;
  onRemove: () => void;
}) {
  return (
    <div
      className={`flex items-center gap-2 rounded border px-2 py-1.5 ${
        selected ? 'border-pc-accent ring-1 ring-pc-accent' : 'border-pc-border'
      }`}
    >
      <button type="button" onClick={onSelect} className="flex min-w-0 flex-1 items-center gap-2 text-left">
        <span className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded bg-pc-accent text-[11px] font-semibold text-[#0b1220]">
          {step.number}
        </span>
        <span className="truncate text-sm text-pc-text">{step.title || t('sops.untitled')}</span>
        {step.kind === 'checkpoint' ? <Badge tone="warn">⏸</Badge> : null}
        {step.calls && step.calls.length > 0 ? (
          <span className="shrink-0 text-[11px] text-pc-text-muted">⚙ {step.calls.length}</span>
        ) : null}
      </button>
      <button
        type="button"
        onClick={() => onMove(-1)}
        disabled={index === 0}
        className="rounded px-1 text-pc-text-muted hover:bg-pc-elevated disabled:opacity-30"
        aria-label={t('sops.move_up')}
      >
        ↑
      </button>
      <button
        type="button"
        onClick={() => onMove(1)}
        disabled={index === count - 1}
        className="rounded px-1 text-pc-text-muted hover:bg-pc-elevated disabled:opacity-30"
        aria-label={t('sops.move_down')}
      >
        ↓
      </button>
      <button
        type="button"
        onClick={onRemove}
        className="rounded px-1 text-status-error hover:bg-pc-elevated"
        aria-label={t('sops.remove_step')}
      >
        <Trash2 className="h-3.5 w-3.5" aria-hidden />
      </button>
    </div>
  );
}

function DraftSidebar({
  draft,
  saving,
  saveError,
  selectedStep,
  selectedTrigger,
  triggerRegistry,
  agentAliases,
  onSelectStep,
  onField,
  onTrigger,
  onAddTrigger,
  onRemoveTrigger,
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
  selectedTrigger: number | null;
  triggerRegistry: TriggerSourceRegistry | null;
  agentAliases: string[];
  onSelectStep: (n: number) => void;
  onField: (patch: Partial<Sop>) => void;
  onTrigger: (i: number, next: SopTrigger) => void;
  onAddTrigger: () => void;
  onRemoveTrigger: (i: number) => void;
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
            className="inline-flex items-center gap-1 rounded bg-pc-accent px-2 py-1 text-sm text-[#0b1220] hover:bg-pc-accent-light disabled:opacity-50"
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
      {saveError ? <div className="text-sm text-status-error">{saveError}</div> : null}
      <TextField
        label={t('sops.field_name')}
        value={draft.name}
        onChange={(v) => onField({ name: v })}
        help={sopFieldHelp('Sop', 'name')}
      />
      <TextField
        label={t('sops.field_description')}
        value={draft.description}
        onChange={(v) => onField({ description: v })}
        help={sopFieldHelp('Sop', 'description')}
      />
      <div className="grid grid-cols-2 gap-3">
        <TextField
          label={t('sops.field_version')}
          value={draft.version}
          onChange={(v) => onField({ version: v })}
          help={sopFieldHelp('Sop', 'version')}
        />
        <SelectField
          label={t('sops.field_priority')}
          value={draft.priority}
          onChange={(v) => onField({ priority: v as Sop['priority'] })}
          options={['critical', 'high', 'normal', 'low']}
          help={sopFieldHelp('Sop', 'priority')}
        />
      </div>
      <SelectField
        label={t('sops.field_execution_mode')}
        value={draft.execution_mode}
        onChange={(v) => onField({ execution_mode: v as Sop['execution_mode'] })}
        options={['auto', 'supervised', 'step_by_step', 'priority_based', 'deterministic']}
        help={sopFieldHelp('Sop', 'execution_mode')}
      />
      <SelectField
        label={t('sops.field_agent')}
        value={draft.agent ?? ''}
        onChange={(v) => onField({ agent: v === '' ? null : v })}
        help={sopFieldHelp('Sop', 'agent')}
      >
        <option value="">{t('sops.agent_none')}</option>
        {agentAliases.map((alias) => (
          <option key={alias} value={alias}>
            {alias}
          </option>
        ))}
      </SelectField>
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <span className="text-sm font-medium text-pc-text">{t('sops.triggers')}</span>
          <button
            type="button"
            onClick={onAddTrigger}
            className="inline-flex items-center gap-1 rounded border border-pc-border px-2 py-1 text-xs text-pc-text hover:bg-pc-elevated"
          >
            <Plus className="h-3.5 w-3.5" aria-hidden /> {t('sops.add_trigger')}
          </button>
        </div>
        {draft.triggers.length === 0 ? (
          <p className="text-xs text-pc-text-muted">{t('sops.trigger_none')}</p>
        ) : (
          draft.triggers.map((trigger, i) => (
            <TriggerEditor
              key={i}
              trigger={trigger}
              index={i}
              selected={selectedTrigger === i}
              registry={triggerRegistry}
              onChange={(next) => onTrigger(i, next)}
              onRemove={() => onRemoveTrigger(i)}
            />
          ))
        )}
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
        {draft.steps.length === 0 ? (
          <p className="text-xs text-pc-text-muted">{t('sops.no_steps')}</p>
        ) : (
          draft.steps.map((s, i) => (
            <StepListRow
              key={i}
              step={s}
              index={i}
              count={draft.steps.length}
              selected={selectedStep === s.number}
              onSelect={() => onSelectStep(s.number)}
              onMove={(dir) => onMoveStep(i, dir)}
              onRemove={() => onRemoveStep(i)}
            />
          ))
        )}
      </div>
    </Card>
  );
}

function StepInspector({
  draft,
  selectedStep,
  runCallsByStep,
  agentAliases,
  onStep,
  onRemoveStep,
  onMoveStep,
}: {
  draft: Sop;
  selectedStep: number | null;
  runCallsByStep: Map<number, StepToolCall[]>;
  agentAliases: string[];
  onStep: (i: number, patch: Partial<SopStep>) => void;
  onRemoveStep: (i: number) => void;
  onMoveStep: (i: number, dir: -1 | 1) => void;
}) {
  const index = draft.steps.findIndex((s) => s.number === selectedStep);
  const step = index >= 0 ? draft.steps[index] : undefined;
  if (!step) {
    return (
      <Card>
        <p className="text-sm text-pc-text-muted">{t('sops.inspector_empty')}</p>
      </Card>
    );
  }
  return (
    <StepEditor
      step={step}
      index={index}
      count={draft.steps.length}
      capturedCalls={runCallsByStep.get(step.number)}
      onChange={(patch) => onStep(index, patch)}
      onRemove={() => onRemoveStep(index)}
      onMove={(dir) => onMoveStep(index, dir)}
      agentAliases={agentAliases}
      parentAgent={draft.agent}
    />
  );
}

const noop = () => {};

export default function Sops() {
  const [sops, setSops] = useState<SopSummary[]>([]);
  const [selected, setSelected] = useState<string>('');
  const [graph, setGraph] = useState<SopGraph | null>(null);
  const [viewSop, setViewSop] = useState<Sop | null>(null);
  const [loading, setLoading] = useState(true);
  const [graphLoading, setGraphLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [runId, setRunId] = useState('');
  const [overlay, setOverlay] = useState<RunOverlay | null>(null);
  const [overlayError, setOverlayError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Sop | null>(loadStoredDraft);
  const [editingName, setEditingName] = useState<string | null>(loadStoredEditingName);
  const [draftGraph, setDraftGraph] = useState<SopGraph | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [layer, setLayer] = useState<'visual' | 'fields'>('visual');
  const [selectedStep, setSelectedStep] = useState<number | null>(null);
  const [selectedTrigger, setSelectedTrigger] = useState<number | null>(null);
  const [triggerRegistry, setTriggerRegistry] = useState<TriggerSourceRegistry | null>(null);
  const [agentAliases, setAgentAliases] = useState<string[]>([]);

  // Mirror the in-progress draft to session storage so navigating away (e.g. to
  // channel setup for an unconfigured trigger) and back does not lose it, and
  // warn on a full tab close/reload while a draft is open.
  useEffect(() => {
    storeDraft(draft);
    if (!draft) return;
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [draft]);

  useEffect(() => {
    storeEditingName(draft ? editingName : null);
  }, [draft, editingName]);

  useEffect(() => {
    let active = true;
    triggerSources()
      .then((reg) => {
        if (active) setTriggerRegistry(reg);
      })
      .catch(() => {
        // Registry is best-effort: the editor still renders bound sources it
        // knows about and simply omits channel aliases if the fetch failed.
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;
    loadAgentPickerSummaries()
      .then((list) => {
        if (active) setAgentAliases(list.map((a) => a.alias));
      })
      .catch(() => {
        // Agent list is best-effort: the selector falls back to free-typed
        // aliases if the fetch failed.
      });
    return () => {
      active = false;
    };
  }, []);

  const onConnect = useCallback(
    (from: number, to: number, kind: WireRole, portIndex?: number) => {
      setDraft((d) => {
        if (!d) return d;
        wireDraft(d, { op: 'connect', from, to, role: kind, port: portIndex })
          .then((res) => {
            setDraft(res.sop);
            setDraftGraph(res.graph);
          })
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
          .then((res) => {
            setDraft(res.sop);
            setDraftGraph(res.graph);
          })
          .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
        return d;
      });
    },
    [],
  );

  const onConnectData = useCallback(
    (fromStep: number, fromPin: string, toStep: number, toPin: string) => {
      const binding = `{{steps.${fromStep}.${fromPin}}}`;
      setDraft((d) => {
        if (!d) return d;
        const next = writeStepBinding(d, toStep, toPin, binding);
        graphDraft(next)
          .then((g) => {
            setDraft(next);
            setDraftGraph(g);
          })
          .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
        return next;
      });
    },
    [],
  );

  const onDisconnectData = useCallback(
    (toStep: number, toPin: string) => {
      setDraft((d) => {
        if (!d) return d;
        const next = writeStepBinding(d, toStep, toPin, null);
        graphDraft(next)
          .then((g) => {
            setDraft(next);
            setDraftGraph(g);
          })
          .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
        return next;
      });
    },
    [],
  );

  // Reproject the draft graph from the backend whenever the draft changes so
  // the canvas reflects trigger fan-in, data wires, pins, and layout without
  // re-deriving graph shape client-side. Single source: `graphDraft`.
  useEffect(() => {
    if (!draft) {
      setDraftGraph(null);
      return;
    }
    let active = true;
    graphDraft(draft)
      .then((g) => {
        if (active) setDraftGraph(g);
      })
      .catch((e: unknown) => {
        if (active) setSaveError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, [draft]);

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

  const startNew = useCallback(() => {
    setSaveError(null);
    setEditingName(null);
    setDraft(blankSop(''));
  }, []);

  const startEdit = useCallback(() => {
    if (!selected) return;
    setSaveError(null);
    getSop(selected)
      .then((full) => {
        setEditingName(full.name);
        setDraft(full);
      })
      .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
  }, [selected]);

  const onSaveDraft = useCallback(() => {
    if (!draft) return;
    setSaving(true);
    setSaveError(null);
    // Save is an upsert: PUT /api/sops/{name} creates the SOP when absent and
    // overwrites it when present, so a save never 409s on its own name. When an
    // existing SOP was renamed in the editor (draft.name diverged from the name
    // the edit started at), the upsert writes the new name; we then delete the
    // old directory so the rename moves rather than forks. Renumbering and
    // routing-ref remapping are owned by the daemon's normalize_step_numbers.
    const renamedFrom =
      editingName !== null && draft.name !== editingName ? editingName : null;
    saveSop(draft)
      .then(() => (renamedFrom ? deleteSop(renamedFrom).catch(() => undefined) : undefined))
      .then(() => {
        setSaving(false);
        setDraft(null);
        setEditingName(null);
        return refreshList(draft.name);
      })
      .catch((e: unknown) => {
        setSaving(false);
        setSaveError(e instanceof Error ? e.message : String(e));
      });
  }, [draft, editingName, refreshList]);

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
    Promise.all([getSopGraph(name), getSop(name)])
      .then(([g, full]) => {
        setGraph(g);
        setViewSop(full);
        setGraphLoading(false);
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
        setGraph(null);
        setViewSop(null);
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

  const runStateByStep = useMemo(() => overlayStateByStep(overlay), [overlay]);
  const runCallsByStep = useMemo(() => overlayCallsByStep(overlay), [overlay]);

  // Keep the inspector pointed at a real step: select the first step when a
  // draft opens and drop the selection when its step is removed.
  useEffect(() => {
    if (!draft) {
      setSelectedStep(null);
      return;
    }
    setSelectedStep((cur) =>
      cur !== null && draft.steps.some((s) => s.number === cur)
        ? cur
        : (draft.steps[0]?.number ?? null),
    );
  }, [draft]);

  const mutateDraft = useCallback((updater: (d: Sop) => Sop) => {
    setSaveError(null);
    setDraft((d) => (d ? updater(d) : d));
  }, []);

  const editorHandlers = draft
    ? {
        onField: (patch: Partial<Sop>) => mutateDraft((d) => ({ ...d, ...patch })),
        onTrigger: (i: number, next: SopTrigger) =>
          mutateDraft((d) => ({
            ...d,
            triggers: d.triggers.map((tr, j) => (j === i ? next : tr)),
          })),
        onAddTrigger: () =>
          mutateDraft((d) => ({
            ...d,
            triggers: [...d.triggers, blankTrigger(MANUAL_SOURCE, triggerRegistry)],
          })),
        onRemoveTrigger: (i: number) =>
          mutateDraft((d) => ({ ...d, triggers: d.triggers.filter((_, j) => j !== i) })),
        onStep: (i: number, patch: Partial<SopStep>) =>
          mutateDraft((d) => ({
            ...d,
            steps: d.steps.map((s, j) => (j === i ? { ...s, ...patch } : s)),
          })),
        onMoveNode: (step: number, x: number, y: number) =>
          mutateDraft((d) => ({
            ...d,
            steps: d.steps.map((s) => (s.number === step ? { ...s, pos: { x, y } } : s)),
          })),
        onAddStep: () =>
          mutateDraft((d) => ({ ...d, steps: [...d.steps, blankStep(d.steps.length + 1)] })),
        onRemoveStep: (i: number) =>
          mutateDraft((d) => ({ ...d, steps: d.steps.filter((_, j) => j !== i) })),
        onMoveStep: (i: number, dir: -1 | 1) =>
          mutateDraft((d) => {
            const j = i + dir;
            if (j < 0 || j >= d.steps.length) return d;
            const steps = [...d.steps];
            [steps[i], steps[j]] = [steps[j]!, steps[i]!];
            return { ...d, steps };
          }),
      }
    : null;

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <PageHeader
        title={t('sops.title')}
        description={t('sops.subtitle')}
        actions={
          !draft ? (
            <button
              type="button"
              onClick={startNew}
              className="inline-flex items-center gap-1 rounded bg-pc-accent px-3 py-1.5 text-sm text-[#0b1220] hover:bg-pc-accent-light"
            >
              <Plus className="h-4 w-4" aria-hidden /> {t('sops.new')}
            </button>
          ) : null
        }
      />
      {error ? (
        <Card>
          <div className="text-status-error">{error}</div>
        </Card>
      ) : null}
      {draft && editorHandlers ? (
        <div className="space-y-4">
          <div className="grid grid-cols-1 items-start gap-4 lg:grid-cols-[20rem_minmax(0,1fr)]">
            <DraftSidebar
              draft={draft}
              saving={saving}
              saveError={saveError}
              selectedStep={selectedStep}
              selectedTrigger={selectedTrigger}
              triggerRegistry={triggerRegistry}
              agentAliases={agentAliases}
              onSelectStep={setSelectedStep}
              onField={editorHandlers.onField}
              onTrigger={editorHandlers.onTrigger}
              onAddTrigger={editorHandlers.onAddTrigger}
              onRemoveTrigger={editorHandlers.onRemoveTrigger}
              onAddStep={editorHandlers.onAddStep}
              onRemoveStep={editorHandlers.onRemoveStep}
              onMoveStep={editorHandlers.onMoveStep}
              onSave={onSaveDraft}
              onCancel={() => {
                setDraft(null);
                setEditingName(null);
              }}
            />
            <StepInspector
              draft={draft}
              selectedStep={selectedStep}
              runCallsByStep={runCallsByStep}
              agentAliases={agentAliases}
              onStep={editorHandlers.onStep}
              onRemoveStep={editorHandlers.onRemoveStep}
              onMoveStep={editorHandlers.onMoveStep}
            />
          </div>
          {draftGraph ? (
            <SopCanvas
              draft={draft}
              graph={draftGraph}
              selectedStep={selectedStep}
              runStateByStep={runStateByStep}
              onSelectStep={setSelectedStep}
              onSelectTrigger={setSelectedTrigger}
              onAddStep={editorHandlers.onAddStep}
              onRemoveStep={(n) => {
                const i = draft.steps.findIndex((s) => s.number === n);
                if (i >= 0) editorHandlers.onRemoveStep(i);
              }}
              onConnect={onConnect}
              onDisconnect={onDisconnect}
              onConnectData={onConnectData}
              onDisconnectData={onDisconnectData}
              onMoveNode={editorHandlers.onMoveNode}
            />
          ) : null}
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
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-[14rem_minmax(0,1fr)]">
          <Card className="h-fit p-2">
            <ul className="space-y-1">
              {sops.map((s) => (
                <li key={s.name}>
                  <button
                    type="button"
                    onClick={() => setSelected(s.name)}
                    className={`w-full rounded px-2 py-1.5 text-left text-sm ${
                      s.name === selected
                        ? 'bg-pc-accent text-[#0b1220]'
                        : 'text-pc-text hover:bg-pc-elevated'
                    }`}
                  >
                    <div className="font-medium">{s.name}</div>
                    {s.description ? (
                      <div
                        className={`truncate text-xs ${
                          s.name === selected ? 'text-[#0b1220]/70' : 'text-pc-text-muted'
                        }`}
                      >
                        {s.description}
                      </div>
                    ) : null}
                  </button>
                </li>
              ))}
            </ul>
          </Card>
          <div>
            <div className="mb-3 flex flex-wrap items-center gap-2">
              <span className="font-medium text-pc-text">{selected}</span>
              {graph ? (
                <Badge tone="neutral">
                  {graph.nodes.length} {t('sops.steps')}
                </Badge>
              ) : null}
              <div className="ml-auto flex flex-wrap gap-2">
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
                  className="inline-flex items-center gap-1 rounded border border-pc-border px-2 py-1 text-sm text-status-error hover:bg-pc-elevated"
                >
                  <Trash2 className="h-4 w-4" aria-hidden /> {t('sops.delete')}
                </button>
              </div>
            </div>
            <div className="mb-3 flex flex-wrap items-center gap-2">
              <input
                type="text"
                value={runId}
                onChange={(e) => setRunId(e.target.value.trim())}
                placeholder={t('sops.run_id_placeholder')}
                className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-sm text-pc-text sm:w-64"
              />
              {overlay ? (
                <Badge tone={overlay.status === 'failed' ? 'error' : 'ok'}>
                  {t(`sops.run_status.${overlay.status}`)} · {overlay.current_step}/
                  {overlay.total_steps}
                </Badge>
              ) : null}
              {overlayError ? <span className="text-xs text-status-error">{overlayError}</span> : null}
            </div>
            {graphLoading ? (
              <Loader2 className="h-5 w-5 animate-spin text-pc-text-muted" aria-hidden />
            ) : graph ? (
              <>
                {layer === 'visual' && viewSop ? (
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
