import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react';
import { Activity, AlertTriangle, Bot, Check, Code2, GitBranch, Loader2, PanelLeft, PanelRight, Plus, Save, Settings2, Timer, Trash2, X, XCircle } from 'lucide-react';
import { Link } from 'react-router-dom';
import { Badge, Card, HelpTip } from '@/components/ui';
import SopSourceEditor from '@/components/SopSourceEditor';
import Code from '@/pages/Code';
import { useWorkspaceVisible } from '@/components/layout/WorkspaceOutlet';
import SopCanvas from './SopCanvas';
import { planSopSave, sopErrorText } from './sopSavePlan';
import MarkdownEditor from '@/components/MarkdownEditor';
import ToolPicker from '@/components/ToolPicker';
import { JsonField, PlannedCallsEditor } from '@/components/SopCalls';
import { SopRunControls, SopRunInspector, SopRunsPanel } from '@/components/SopRunPanel';
import { useRunOverlay } from '@/hooks/useRunOverlay';
import { isSopDraft } from '@/lib/sopDraft';
import { t } from '@/lib/i18n';
import { loadAgentPickerSummaries } from '@/lib/agents';
import {
  listRuns,
  getRunOverlay,
  getSopGraph,
  overlayStateByStep,
  getSop,
  runSop,
  createSop,
  saveSop,
  renameSop,
  decisionModels,
  sopDecisionModes,
  type DecisionModelOption,
  type SopDecisionSpec,
  wireDraft,
  graphDraft,
  triggerSources,
  sopFieldHelp,
  overlayCallsByStep,
  parseCondition,
  buildCondition,
  sopPriorities,
  sopExecutionModes,
  sopStepKinds,
  type WireRole,
  type SopGraph,
  type RunOverlay,
  type Sop,
  type SopStep,
  type SopTrigger,
  type SopRunSummary,
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

function draftStorageKey(name: string | null) { return `${DRAFT_STORAGE_KEY}:${name ?? '@new'}`; }
function loadStoredDraft(name: string | null): Sop | null {
  try {
    const raw = sessionStorage.getItem(draftStorageKey(name));
    if (raw) return JSON.parse(raw) as Sop;
    // Recover drafts made before drafts were scoped to each carousel item.
    if (sessionStorage.getItem(DRAFT_EDITING_NAME_KEY) === name) {
      const legacy = sessionStorage.getItem(DRAFT_STORAGE_KEY);
      if (legacy) return JSON.parse(legacy) as Sop;
    }
  } catch { /* Optional recovery; the gateway remains the saved owner. */ }
  return null;
}
function storeDraft(name: string | null, draft: Sop | null): void {
  try {
    if (draft) sessionStorage.setItem(draftStorageKey(name), JSON.stringify(draft));
    else sessionStorage.removeItem(draftStorageKey(name));
    if (sessionStorage.getItem(DRAFT_EDITING_NAME_KEY) === name) {
      sessionStorage.removeItem(DRAFT_STORAGE_KEY);
      sessionStorage.removeItem(DRAFT_EDITING_NAME_KEY);
    }
  } catch { /* Optional draft recovery. */ }
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
    admission_policy: 'parallel',
    max_pending_approvals: 0,
    deterministic: false,
  };
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
        aria-label={label}
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
  options?: readonly string[];
  disabled?: boolean;
  children?: ReactNode;
  help?: string | null;
}) {
  return (
    <Field label={label} help={help}>
      <select
        aria-label={label}
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
  hasDecision,
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
  hasDecision: boolean;
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
      <div className="mb-2 flex flex-wrap items-center gap-2">
        <HelpTip text={sopFieldHelp('SopStep', 'number')}>
          <span className="inline-flex h-6 w-6 items-center justify-center rounded bg-pc-accent text-xs font-semibold text-[#0b1220]">
            {step.number}
          </span>
        </HelpTip>
        <select
          value={step.kind ?? 'execute'}
          onChange={(e) => onChange({ kind: e.target.value as SopStep['kind'] })}
          className="mr-auto rounded border border-pc-border bg-pc-surface px-1.5 py-1 text-xs text-pc-text"
          aria-label={t('sops.step_kind')}
          title={sopFieldHelp('SopStep', 'kind') ?? undefined}
        >
          {sopStepKinds.map((kind) => (
            <option key={kind} value={kind}>
              {t(`sops.kind_${kind}`)}
            </option>
          ))}
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
      <div className="mb-4">
        <input
          type="text"
          value={step.title}
          onChange={(e) => onChange({ title: e.target.value })}
          placeholder={t('sops.step_title_placeholder')}
          aria-label={t('sops.step_title_placeholder')}
          title={sopFieldHelp('SopStep', 'title') ?? undefined}
          className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-sm text-pc-text"
        />
      </div>
      {step.kind === 'capability' && (
        <div className="mb-4 space-y-3">
          <TextField
            label={t('sop_workspace.capability')}
            value={step.capability ?? ''}
            onChange={(value) => onChange({ capability: value })}
            help={sopFieldHelp('SopStep', 'capability')}
          />
          <JsonField
            label={t('sop_workspace.capability_input')}
            value={step.with ?? {}}
            onChange={(value) => onChange({ with: value })}
          />
        </div>
      )}
      {step.kind === 'checkpoint' && (
        <div className="mb-4 space-y-3">
          <TextField
            label={t('sop_workspace.approval_prompt')}
            value={step.gate_prompt ?? ''}
            onChange={(value) => onChange({ gate_prompt: value || null })}
            help={sopFieldHelp('SopStep', 'gate_prompt')}
          />
          <TextField
            label={t('sop_workspace.approval_policy')}
            value={step.policy ?? ''}
            onChange={(value) => onChange({ policy: value || null })}
            help={sopFieldHelp('SopStep', 'policy')}
          />
        </div>
      )}
      <div className="mb-2">
        <StepBodyEditor value={step.body} onChange={(next) => onChange({ body: next })} />
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
      {hasDecision || step.decide || step.unless_decided ? (
        <div className="mb-2 grid grid-cols-3 gap-2 border-t border-pc-border pt-2 text-xs">
          <div className="col-span-2">
            <TextField
              label={t('sops.step_decide')}
              value={step.decide ?? ''}
              placeholder={t('sops.step_decide_placeholder')}
              help={sopFieldHelp('SopStep', 'decide')}
              onChange={(v) => onChange({ decide: v.trim() === '' ? null : v })}
            />
          </div>
          <Field label={t('sops.step_unless_decided')} help={sopFieldHelp('SopStep', 'unless_decided')}>
            <input
              type="number"
              min={1}
              value={step.unless_decided ?? ''}
              onChange={(e) =>
                onChange({
                  unless_decided: e.target.value ? parseInt(e.target.value, 10) : null,
                })
              }
              placeholder="—"
              className={INPUT_CLS}
            />
          </Field>
        </div>
      ) : null}
      <details
        open={Boolean(step.routing || step.on_failure)}
        className="mt-4 border-t border-pc-border pt-3"
      >
        <summary className="mb-3 cursor-pointer text-sm font-medium">
          {t('sop_workspace.routing_settings')}
        </summary>
        <div className="grid grid-cols-2 gap-3 text-xs">
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
          {fkind === 'retry' &&
          step.on_failure &&
          typeof step.on_failure === 'object' &&
          'retry' in step.on_failure ? (
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
          {fkind === 'goto' &&
          step.on_failure &&
          typeof step.on_failure === 'object' &&
          'goto' in step.on_failure ? (
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
              <HelpTip text={sopFieldHelp('StepRouting', 'switch')}>
                {t('sops.switch_ports')}
              </HelpTip>
            </span>
            <button
              type="button"
              onClick={() =>
                setRouting({
                  switch: [
                    ...(routing.switch ?? []),
                    {
                      name: `port ${(routing.switch?.length ?? 0) + 1}`,
                      when: undefined,
                      goto: undefined,
                    },
                  ],
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
                <div
                  key={ri}
                  className="mb-3 grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_1.5rem] items-center gap-2"
                >
                  <input
                    type="text"
                    value={rule.name}
                    onChange={(e) => setRule({ name: e.target.value })}
                    placeholder={t('sops.port_name')}
                    className="min-w-0 rounded border border-pc-border bg-pc-surface px-1.5 py-0.5 text-xs text-pc-text"
                  />
                  <input
                    type="text"
                    value={rule.when ?? ''}
                    onChange={(e) => setRule({ when: e.target.value || undefined })}
                    placeholder={t('sops.port_when')}
                    className="col-span-3 row-start-2 min-w-0 rounded border border-pc-border bg-pc-surface px-1.5 py-0.5 text-xs text-pc-text"
                  />
                  <input
                    type="number"
                    value={rule.goto ?? ''}
                    onChange={(e) =>
                      setRule({ goto: e.target.value ? parseInt(e.target.value, 10) : undefined })
                    }
                    placeholder="→"
                    className="min-w-0 rounded border border-pc-border bg-pc-surface px-1.5 py-0.5 text-xs text-pc-text"
                  />
                  <button
                    type="button"
                    onClick={() =>
                      setRouting({ switch: (routing.switch ?? []).filter((_, j) => j !== ri) })
                    }
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
      </details>
      <div className="mt-2">
        <PlannedCallsEditor
          calls={step.calls ?? []}
          captured={capturedCalls}
          agent={step.agent ?? parentAgent}
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
/// No operator or path is typed blind. Every channel and known-shape source
/// enumerates its fields, so the builder renders a field picker. Only genuinely
/// arbitrary payloads (mqtt, amqp) mark `open` and fall back to a free input
/// with an advanced raw-string escape hatch; `direct` scalar payloads drop the
/// path entirely. Sources with no contract render nothing (condition
/// unsupported).
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

function DecisionEditor({
  decision,
  deterministic,
  models,
  onChange,
}: {
  decision: SopDecisionSpec | null | undefined;
  deterministic: boolean;
  models: DecisionModelOption[];
  onChange: (next: SopDecisionSpec | null) => void;
}) {
  const help = (field: string) => sopFieldHelp('SopDecisionSpec', field);
  const enable = () =>
    onChange({
      model: models[0]?.alias ?? '',
      gate: null,
      gate_threshold: 0.7,
      gate_on_error: 'run_strict',
      modes: [],
      mode_instructions: null,
      min_confidence: 0.7,
      part_threshold: 0.5,
    });
  const set = (patch: Partial<SopDecisionSpec>) => {
    if (decision) onChange({ ...decision, ...patch });
  };
  const modes = decision?.modes ?? [];
  const toggleMode = (mode: string, on: boolean) =>
    set({
      modes: sopDecisionModes.filter((m) => (m === mode ? on : modes.includes(m))),
    });
  const probability = (v: string) => {
    const n = parseFloat(v);
    return Number.isFinite(n) ? n : 0;
  };
  const configured = decision ? models.some((m) => m.alias === decision.model) : true;

  return (
    <div className="space-y-2 rounded border border-pc-border p-2">
      <label className="flex items-center gap-2 text-sm font-medium text-pc-text">
        <input
          type="checkbox"
          checked={decision != null}
          onChange={(e) => (e.target.checked ? enable() : onChange(null))}
        />
        <HelpTip text={t('sops.decision_help')}>{t('sops.decision_title')}</HelpTip>
      </label>
      {decision ? (
        <>
          <SelectField
            label={t('sops.decision_model')}
            value={decision.model}
            onChange={(v) => set({ model: v })}
            help={help('model')}
          >
            {models.length === 0 ? <option value="">{t('sops.decision_no_models')}</option> : null}
            {!configured && decision.model ? (
              <option value={decision.model}>
                {decision.model} {t('sops.decision_not_configured')}
              </option>
            ) : null}
            {models.map((m) => (
              <option key={m.alias} value={m.alias}>
                {m.alias} ({m.provider}: {m.model})
              </option>
            ))}
          </SelectField>
          {!configured ? (
            <p className="text-xs text-status-warning">{t('sops.decision_not_configured_hint')}</p>
          ) : null}
          <TextField
            label={t('sops.decision_gate')}
            value={decision.gate ?? ''}
            placeholder={t('sops.decision_gate_placeholder')}
            onChange={(v) => set({ gate: v.trim() === '' ? null : v })}
            help={help('gate')}
          />
          <div className="grid grid-cols-2 gap-3">
            <Field label={t('sops.decision_gate_threshold')} help={help('gate_threshold')}>
              <input
                type="number"
                min={0}
                max={1}
                step={0.05}
                value={decision.gate_threshold ?? 0.7}
                onChange={(e) => set({ gate_threshold: probability(e.target.value) })}
                className={INPUT_CLS}
              />
            </Field>
            <SelectField
              label={t('sops.decision_gate_on_error')}
              value={decision.gate_on_error ?? 'run_strict'}
              onChange={(v) => set({ gate_on_error: v as SopDecisionSpec['gate_on_error'] })}
              help={help('gate_on_error')}
            >
              <option value="run_strict">{t('sops.decision_on_error_run_strict')}</option>
              <option value="skip">{t('sops.decision_on_error_skip')}</option>
            </SelectField>
          </div>
          <Field
            label={t('sops.decision_modes')}
            help={help('modes')}
            hint={deterministic ? t('sops.decision_modes_deterministic') : null}
          >
            <div className="flex flex-wrap gap-3 text-sm text-pc-text">
              {sopDecisionModes.map((mode) => (
                <label key={mode} className="flex items-center gap-1">
                  <input
                    type="checkbox"
                    disabled={deterministic}
                    checked={modes.includes(mode)}
                    onChange={(e) => toggleMode(mode, e.target.checked)}
                  />
                  {mode}
                </label>
              ))}
            </div>
          </Field>
          {modes.length > 0 ? (
            <>
              <TextField
                label={t('sops.decision_mode_instructions')}
                value={decision.mode_instructions ?? ''}
                placeholder={t('sops.decision_mode_instructions_placeholder')}
                onChange={(v) => set({ mode_instructions: v.trim() === '' ? null : v })}
                help={help('mode_instructions')}
              />
              <Field label={t('sops.decision_min_confidence')} help={help('min_confidence')}>
                <input
                  type="number"
                  min={0}
                  max={1}
                  step={0.05}
                  value={decision.min_confidence ?? 0.7}
                  onChange={(e) => set({ min_confidence: probability(e.target.value) })}
                  className={INPUT_CLS}
                />
              </Field>
            </>
          ) : null}
          <Field
            label={t('sops.decision_part_threshold')}
            help={help('part_threshold')}
            hint={t('sops.decision_parts_hint')}
          >
            <input
              type="number"
              min={0}
              max={1}
              step={0.05}
              value={decision.part_threshold ?? 0.5}
              onChange={(e) => set({ part_threshold: probability(e.target.value) })}
              className={INPUT_CLS}
            />
          </Field>
        </>
      ) : null}
    </div>
  );
}

function DraftSidebar({
  draft,
  selectedStep,
  selectedTrigger,
  triggerRegistry,
  agentAliases,
  decisionModelOptions,
  onSelectStep,
  onField,
  onTrigger,
  onAddTrigger,
  onRemoveTrigger,
  onAddStep,
  onRemoveStep,
  onMoveStep,
}: {
  draft: Sop;
  selectedStep: number | null;
  selectedTrigger: number | null;
  triggerRegistry: TriggerSourceRegistry | null;
  agentAliases: string[];
  decisionModelOptions: DecisionModelOption[];
  onSelectStep: (n: number) => void;
  onField: (patch: Partial<Sop>) => void;
  onTrigger: (i: number, next: SopTrigger) => void;
  onAddTrigger: () => void;
  onRemoveTrigger: (i: number) => void;
  onAddStep: () => void;
  onRemoveStep: (i: number) => void;
  onMoveStep: (i: number, dir: -1 | 1) => void;
}) {
  return (
    <div className="space-y-4">
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
          options={sopPriorities}
          help={sopFieldHelp('Sop', 'priority')}
        />
      </div>
      <SelectField
        label={t('sops.field_execution_mode')}
        value={draft.execution_mode}
        onChange={(v) => onField({ execution_mode: v as Sop['execution_mode'] })}
        options={sopExecutionModes}
        help={sopFieldHelp('Sop', 'execution_mode')}
      />
      <p className="text-xs leading-relaxed text-pc-text-muted">{t(`sop_workspace.mode_${draft.execution_mode}`)}</p>
      <DecisionEditor
        decision={draft.decision}
        deterministic={draft.deterministic ?? false}
        models={decisionModelOptions}
        onChange={(next) => onField({ decision: next })}
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
    </div>
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
      key={step.number}
      step={step}
      index={index}
      count={draft.steps.length}
      capturedCalls={runCallsByStep.get(step.number)}
      onChange={(patch) => onStep(index, patch)}
      onRemove={() => onRemoveStep(index)}
      onMove={(dir) => onMoveStep(index, dir)}
      agentAliases={agentAliases}
      parentAgent={draft.agent}
      hasDecision={draft.decision != null}
    />
  );
}



/// Build a Manual-run payload skeleton from a SOP's step-1 input JSON Schema.
/// Registry-driven: keys and placeholder value shapes come from the SOP's own
/// declared `schema.input`, never a hardcoded per-SOP template.
function payloadSkeleton(sop: Sop | null): string {
  const input = sop?.steps?.find((s) => s.number === 1)?.schema?.input;
  const props =
    input && typeof input === 'object' && !Array.isArray(input)
      ? (input as { properties?: Record<string, unknown> }).properties
      : undefined;
  if (!props || typeof props !== 'object') return '{}';
  const skeleton: Record<string, unknown> = {};
  for (const [key, spec] of Object.entries(props)) {
    const type =
      spec && typeof spec === 'object' && !Array.isArray(spec)
        ? (spec as { type?: string }).type
        : undefined;
    skeleton[key] = placeholderForType(type);
  }
  return JSON.stringify(skeleton, null, 2);
}

function placeholderForType(type: string | undefined): unknown {
  switch (type) {
    case 'number':
    case 'integer':
      return 0;
    case 'boolean':
      return false;
    case 'array':
      return [];
    case 'object':
      return {};
    default:
      return '';
  }
}

/// Manual-run affordance for a SOP that declares a manual trigger. Fires
/// POST /api/sops/{name}/run and selects its overlay in the workspace.
function ManualRunPanel({ name, sop, onStarted, disabled }: { name: string; sop: Sop | null; onStarted: (run: string) => void; disabled: boolean }) {
  const [payload, setPayload] = useState('');
  const [running, setRunning] = useState(false);
  const [runError, setRunError] = useState<string | null>(null);

  const hasManualTrigger = useMemo(
    () => (sop?.triggers ?? []).some((tr) => tr.type === 'manual'),
    [sop],
  );

  useEffect(() => {
    if (!hasManualTrigger) {
      setPayload('');
      return;
    }
    setPayload((cur) => (cur.trim() ? cur : payloadSkeleton(sop)));
  }, [hasManualTrigger, sop]);

  const onRun = useCallback(() => {
    const trimmed = payload.trim();
    if (trimmed) {
      try {
        JSON.parse(trimmed);
      } catch {
        setRunError(t('sop_workspace.invalid_payload'));
        return;
      }
    }
    setRunning(true);
    setRunError(null);
    runSop(name, trimmed || undefined)
      .then(({ run_id }) =>
        onStarted(run_id),
      )
      .catch((e: unknown) => setRunError(`${t('sops.run_error')}: ${String(e)}`))
      .finally(() => setRunning(false));
  }, [name, payload, onStarted]);

  if (!hasManualTrigger) return null;

  return (
    <Card className="space-y-2">
      <textarea
        value={payload}
        onChange={(e) => setPayload(e.target.value)}
        placeholder={t('sops.run_payload_placeholder')}
        rows={4}
        className="w-full rounded border border-pc-border bg-pc-surface px-2 py-1 font-mono text-xs text-pc-text"
      />
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={onRun}
          disabled={running || disabled}
          className="inline-flex items-center gap-1 rounded border border-pc-border bg-pc-accent px-3 py-1 text-sm font-medium text-[#0b1220] hover:opacity-90 disabled:opacity-40"
        >
          {running ? <Loader2 className="h-4 w-4 animate-spin" aria-hidden /> : null}
          {t('sops.run')}
        </button>
        {runError ? <span className="text-xs text-status-error">{runError}</span> : null}
      </div>
    </Card>
  );
}

// The focused editor owns one SOP draft; the workspace preserves it while
// another SOP is selected. Fields, canvas, and applied source edit this draft.
export function SopEditor({
  editing,
  runId,
  runs,
  runError,
  onSaved,
  onSelectRun,
  onEdit,
  libraryOpen,
  onToggleLibrary,
}: {
  editing: string | null;
  runId: string | null;
  runs: SopRunSummary[];
  runError: string;
  onSaved: (name: string) => void;
  onSelectRun: (name: string, run: string) => void;
  onEdit: () => void;
  libraryOpen: boolean;
  onToggleLibrary: () => void;
}) {
  const visible = useWorkspaceVisible();
  const panelId = useId();
  const [draft, setDraft] = useState<Sop | null>(() => loadStoredDraft(editing));
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const [baseline, setBaseline] = useState<string | null>(null);
  const [draftGraph, setDraftGraph] = useState<SopGraph | null>(null);
  const [graphError, setGraphError] = useState('');
  const [saveError, setSaveError] = useState('');
  const [saving, setSaving] = useState(false);
  const [assistantBusy, setAssistantBusy] = useState(false);
  const [sourceDirty, setSourceDirty] = useState(false);
  const [sourceOpen, setSourceOpen] = useState(false);
  const sourceDialog = useRef<HTMLDialogElement>(null);
  const palette = useRef<HTMLDetailsElement>(null);
  const [dockOpen, setDockOpen] = useState(editing === null);
  const [dockTab, setDockTab] = useState<'runs' | 'node' | 'agent'>(
    editing === null ? 'agent' : 'node',
  );
  const [assistantOpened, setAssistantOpened] = useState(editing === null);
  const [inspector, setInspector] = useState<'settings' | 'step' | 'trigger'>('settings');
  const [selectedStep, setSelectedStep] = useState<number | null>(null);
  const [selectedTrigger, setSelectedTrigger] = useState<number | null>(null);
  const [triggerRegistry, setTriggerRegistry] = useState<TriggerSourceRegistry | null>(null);
  const [agentAliases, setAgentAliases] = useState<string[]>([]);
  const [undoStack, setUndoStack] = useState<Sop[]>([]);
  const [decisionModelOptions, setDecisionModelOptions] = useState<DecisionModelOption[]>([]);
  const [latestOverlay, setLatestOverlay] = useState<RunOverlay | null>(null);
  const [runDefinition, setRunDefinition] = useState<{
    id: string;
    sop: Sop;
    graph: SopGraph;
  } | null>(null);
  const [runLoadError, setRunLoadError] = useState('');
  const { overlay, error: overlayError, setOverlay } = useRunOverlay(editing ?? '', runId ?? '');
  const dirty = draft !== null && JSON.stringify(draft) !== baseline;
  const watching = !!runId;
  const runGraph = runDefinition?.id === runId ? runDefinition.graph : null;
  const runSopDefinition = runDefinition?.id === runId ? runDefinition.sop : null;
  const capturedCalls = useMemo(() => overlayCallsByStep(latestOverlay), [latestOverlay]);
  const runStates = useMemo(() => overlayStateByStep(overlay), [overlay]);

  const openDock = (tab: typeof dockTab) => {
    setDockTab(tab);
    setDockOpen(true);
    if (tab === 'agent') setAssistantOpened(true);
  };
  const selectStep = (step: number) => {
    setSelectedStep(step);
    setSelectedTrigger(null);
    setInspector('step');
    openDock('node');
  };
  const selectTrigger = (index: number) => {
    setSelectedTrigger(index);
    setSelectedStep(null);
    setInspector('trigger');
    openDock('node');
  };

  useEffect(() => {
    let active = true;
    const load = editing === null ? Promise.resolve(blankSop('')) : getSop(editing);
    void load
      .then((sop) => {
        if (!active) return;
        setBaseline(JSON.stringify(sop));
        setDraft((current) => current ?? sop);
      })
      .catch((e: unknown) => {
        if (active) setSaveError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, [editing]);

  useEffect(() => {
    if (baseline !== null) storeDraft(editing, dirty ? draft : null);
    if (!dirty && !sourceDirty) return;
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [draft, dirty, sourceDirty, baseline, editing]);

  useEffect(() => {
    let active = true;
    decisionModels()
      .then((res) => {
        if (active) setDecisionModelOptions(res.models);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;
    void triggerSources()
      .then((reg) => {
        if (active) setTriggerRegistry(reg);
      })
      .catch((e: unknown) => {
        if (active) setGraphError(String(e));
      });
    void loadAgentPickerSummaries()
      .then((list) => {
        if (active) setAgentAliases(list.map((agent) => agent.alias));
      })
      .catch((e: unknown) => {
        if (active) setGraphError(String(e));
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!draft) return;
    let active = true;
    const timer = setTimeout(() => {
      void graphDraft(draft)
        .then((graph) => {
          if (active) {
            setDraftGraph(graph);
            setGraphError('');
          }
        })
        .catch((e: unknown) => {
          if (active) setGraphError(e instanceof Error ? e.message : String(e));
        });
    }, 150);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [draft]);

  useEffect(() => {
    if (!editing || !runId) return;
    let active = true;
    setRunLoadError('');
    void Promise.all([getSop(editing), getSopGraph(editing)])
      .then(([sop, graph]) => {
        if (active) setRunDefinition({ id: runId, sop, graph });
      })
      .catch((e: unknown) => {
        if (active) setRunLoadError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      active = false;
    };
  }, [editing, runId]);

  useEffect(() => {
    if (!editing) return;
    let active = true;
    void listRuns(editing)
      .then((items) => {
        const latest = items.sort((a, b) => b.started_at.localeCompare(a.started_at))[0];
        return latest ? getRunOverlay(editing, latest.run_id) : null;
      })
      .then((value) => {
        if (active) setLatestOverlay(value);
      })
      .catch(() => {
        /* Run samples are optional authoring aids. */
      });
    return () => {
      active = false;
    };
  }, [editing]);

  useEffect(() => {
    const dialog = sourceDialog.current;
    if (!dialog) return;
    if (sourceOpen && visible) dialog.showModal();
    else dialog.close();
  }, [sourceOpen, visible]);

  const commitDraft = useCallback((next: Sop) => {
    const current = draftRef.current;
    if (current) setUndoStack((stack) => [...stack.slice(-49), current]);
    draftRef.current = next;
    setDraft(next);
    setSaveError('');
  }, []);
  const mutateDraft = (update: (sop: Sop) => Sop) => {
    if (draftRef.current && !saving && !sourceDirty) commitDraft(update(draftRef.current));
  };
  const undo = useCallback(() => {
    const previous = undoStack[undoStack.length - 1];
    if (!previous || saving || sourceDirty || watching) return;
    draftRef.current = previous;
    setDraft(previous);
    setUndoStack((stack) => stack.slice(0, -1));
    setSaveError('');
  }, [undoStack, saving, sourceDirty, watching]);
  useEffect(() => {
    if (!visible) return;
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target?.matches('input, textarea') || target?.isContentEditable || sourceOpen) return;
      if (e.key.toLowerCase() === 'z' && (e.ctrlKey || e.metaKey) && !e.shiftKey) {
        e.preventDefault();
        undo();
      }
      if (e.key === 'Escape') setDockOpen(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [visible, undo, sourceOpen]);

  const editWire = (
    from: number,
    to: number,
    role: WireRole,
    op: 'connect' | 'disconnect',
    port?: number,
  ) => {
    const original = draftRef.current;
    if (!original || saving || sourceDirty) return;
    void wireDraft(original, { op, from, to, role, port })
      .then((result) => {
        if (draftRef.current !== original) throw new Error(t('workspace.source_conflict'));
        commitDraft(result.sop);
        setDraftGraph(result.graph);
      })
      .catch((e: unknown) => setSaveError(e instanceof Error ? e.message : String(e)));
  };
  const applyProposal = async (source: string) => {
    const original = draftRef.current;
    if (!original || saving || sourceDirty) throw new Error(t('workspace.apply_source_first'));
    const value: unknown = JSON.parse(source);
    if (!isSopDraft(value)) throw new Error(t('workspace.invalid_sop'));
    const next = { ...original, ...value };
    const graph = await graphDraft(next);
    const errors = graph.diagnostics.filter((item) => item.severity === 'error');
    if (errors.length) throw new Error(errors.map((item) => item.message).join('\n'));
    if (draftRef.current !== original) throw new Error(t('workspace.source_conflict'));
    commitDraft(next);
    setDraftGraph(graph);
  };
  const save = async () => {
    const current = draftRef.current;
    if (!current || saving || sourceDirty || assistantBusy) return;
    setSaving(true);
    setSaveError('');
    try {
      const plan = planSopSave(editing, current.name);
      if (plan.kind === 'create') await createSop(current);
      else if (plan.kind === 'save') await saveSop(current);
      else {
        await saveSop({ ...current, name: plan.from });
        try {
          await renameSop(plan.from, plan.to);
        } catch (error) {
          setSaveError(`${t('sops.rename_failed').replace('{name}', plan.from)} ${sopErrorText(error)}`);
          return;
        }
      }
      // Saving normalizes step numbers and bindings; display the persisted
      // definition before enabling a run instead of retaining stale ordinals.
      try {
        const saved = await getSop(current.name);
        draftRef.current = saved;
        setDraft(saved);
        setBaseline(JSON.stringify(saved));
        setUndoStack([]);
        storeDraft(editing, null);
      } catch {
        setBaseline(null);
        setSaveError(t('sop_workspace.save_reload_error'));
      }
      onSaved(current.name);
    } catch (e) {
      setSaveError(sopErrorText(e));
    } finally {
      setSaving(false);
    }
  };
  const reset = async () => {
    setSaving(true);
    setSaveError('');
    try {
      const saved = editing === null ? blankSop('') : await getSop(editing);
      commitDraft(saved);
      setBaseline(JSON.stringify(saved));
      storeDraft(editing, null);
    } catch (e) {
      setSaveError(sopErrorText(e));
    } finally {
      setSaving(false);
    }
  };
  const addNode = (kind: 'agent' | 'tool' | 'branch' | 'approval' | 'wait') => {
    if (!draft) return;
    const number = Math.max(0, ...draft.steps.map((step) => step.number)) + 1;
    const step = {
      ...blankStep(number),
      title: t(`sop_workspace.node_${kind}`),
    };
    if (kind === 'approval') step.kind = 'checkpoint';
    if (kind === 'branch') step.routing = { switch: [{ name: t('sop_workspace.branch_default') }] };
    if (kind === 'tool') step.calls = [{ tool: '', args: {} }];
    if (kind === 'wait') {
      step.kind = 'capability';
      step.capability = 'wait';
      step.with = { seconds: 1 };
    }
    mutateDraft((sop) => ({ ...sop, steps: [...sop.steps, step] }));
    selectStep(number);
    if (palette.current) palette.current.open = false;
  };
  const handlers = {
    onField: (patch: Partial<Sop>) => mutateDraft((sop) => ({ ...sop, ...patch })),
    onStep: (index: number, patch: Partial<SopStep>) =>
      mutateDraft((sop) => ({
        ...sop,
        steps: sop.steps.map((step, i) => (i === index ? { ...step, ...patch } : step)),
      })),
    onTrigger: (index: number, value: SopTrigger) =>
      mutateDraft((sop) => ({
        ...sop,
        triggers: sop.triggers.map((trigger, i) => (i === index ? value : trigger)),
      })),
    onAddTrigger: () =>
      mutateDraft((sop) => ({
        ...sop,
        triggers: [...sop.triggers, blankTrigger(MANUAL_SOURCE, triggerRegistry)],
      })),
    onRemoveTrigger: (index: number) =>
      mutateDraft((sop) => ({
        ...sop,
        triggers: sop.triggers.filter((_, i) => i !== index),
      })),
    onRemoveStep: (index: number) => {
      mutateDraft((sop) => ({
        ...sop,
        steps: sop.steps.filter((_, i) => i !== index),
      }));
      setInspector('settings');
      setSelectedStep(null);
    },
    onMoveStep: (index: number, direction: -1 | 1) =>
      mutateDraft((sop) => {
        const next = index + direction;
        if (next < 0 || next >= sop.steps.length) return sop;
        const steps = [...sop.steps];
        [steps[index], steps[next]] = [steps[next]!, steps[index]!];
        return { ...sop, steps };
      }),
  };
  if (!draft)
    return (
      <div className="p-6">
        {saveError ? (
          <p role="alert" className="text-sm text-status-error">
            {saveError}
          </p>
        ) : (
          <Loader2 className="h-5 w-5 animate-spin" />
        )}
      </div>
    );
  const shownGraph = watching ? runGraph : draftGraph;
  const shownSop = watching ? runSopDefinition : draft;
  const currentTrigger = selectedTrigger === null ? null : draft.triggers[selectedTrigger];
  const latestRun = runs
    .filter((run) => run.sop_name === editing)
    .sort((a, b) => b.started_at.localeCompare(a.started_at))[0];
  const nodeOptions = [
    { kind: 'agent', icon: Bot },
    { kind: 'tool', icon: Code2 },
    { kind: 'branch', icon: GitBranch },
    { kind: 'approval', icon: Check },
    ...(draft.execution_mode === 'deterministic' || draft.deterministic
      ? [{ kind: 'wait', icon: Timer }]
      : []),
  ] as const;

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      <header className="flex shrink-0 flex-wrap items-center gap-2 border-b border-pc-border px-3 py-3 sm:px-5">
        <button
          type="button"
          onClick={onToggleLibrary}
          aria-label={t('sop_workspace.toggle_library')}
          aria-expanded={libraryOpen}
          className="rounded-lg p-2 text-pc-text-muted hover:bg-pc-elevated"
        >
          <PanelLeft className="h-4 w-4" />
        </button>
        <div className="mr-auto min-w-0 flex-1 basis-[calc(100%-3rem)] sm:basis-auto">
          <h1 className="truncate text-sm font-semibold">{draft.name || t('sops.new')}</h1>
          <p className="mt-0.5 text-[11px] text-pc-text-muted">
            {t(
              watching
                ? 'sop_workspace.run_view'
                : dirty
                  ? 'sop_workspace.unsaved'
                  : 'sop_workspace.saved',
            )}
          </p>
        </div>
        <div
          className="flex rounded-lg bg-pc-elevated p-1"
          role="group"
          aria-label={t('sop_workspace.view')}
        >
          <button
            type="button"
            aria-pressed={!watching}
            onClick={onEdit}
            className={`rounded-md px-3 py-1.5 text-xs ${!watching ? 'bg-pc-surface shadow-sm' : 'text-pc-text-muted'}`}
          >
            {t('sop_workspace.edit')}
          </button>
          <button
            type="button"
            aria-pressed={watching}
            onClick={() => {
              if (editing && latestRun) onSelectRun(editing, latestRun.run_id);
              else openDock('runs');
            }}
            className={`rounded-md px-3 py-1.5 text-xs ${watching ? 'bg-pc-surface shadow-sm' : 'text-pc-text-muted'}`}
          >
            {t('sop_workspace.run_view')}
          </button>
        </div>
        {!watching && (
          <button
            type="button"
            disabled={saving || sourceDirty || !dirty || assistantBusy}
            onClick={() => void save()}
            className="btn-primary inline-flex items-center gap-1.5 px-3 py-2 text-xs disabled:opacity-40"
          >
            <Save className="h-3.5 w-3.5" />
            {t(saving ? 'common.loading' : 'sops.save')}
          </button>
        )}
        <button
          type="button"
          aria-label={t('sop_workspace.toggle_panel')}
          aria-expanded={dockOpen}
          onClick={() => setDockOpen((open) => !open)}
          className="rounded-lg p-2 text-pc-text-muted hover:bg-pc-elevated"
        >
          <PanelRight className="h-4 w-4" />
        </button>
      </header>
      <div className="relative flex min-h-0 flex-1">
        <section
          aria-label={t('sop_workspace.canvas')}
          className="flex min-h-0 min-w-0 flex-1 flex-col"
        >
          <div className="flex shrink-0 flex-wrap items-center gap-2 px-4 py-3">
            {!watching && (
              <details ref={palette} className="relative z-10">
                <summary className="btn-secondary flex cursor-pointer list-none items-center gap-1.5 px-3 py-2 text-xs">
                  <Plus className="h-3.5 w-3.5" />
                  {t('sop_workspace.add_node')}
                </summary>
                <div className="absolute left-0 top-full mt-2 w-72 max-w-[85vw] rounded-xl border border-pc-border bg-pc-surface p-2 shadow-xl">
                  {nodeOptions.map(({ kind, icon: Icon }) => (
                    <button
                      key={kind}
                      type="button"
                      disabled={saving || sourceDirty}
                      onClick={() =>
                        addNode(kind as 'agent' | 'tool' | 'branch' | 'approval' | 'wait')
                      }
                      className="flex w-full gap-3 rounded-lg p-3 text-left hover:bg-pc-elevated disabled:opacity-40"
                    >
                      <Icon className="mt-0.5 h-4 w-4 shrink-0 text-pc-accent" />
                      <span>
                        <span className="block text-sm font-medium">
                          {t(`sop_workspace.node_${kind}`)}
                        </span>
                        <span className="mt-1 block text-xs leading-relaxed text-pc-text-muted">
                          {t(`sop_workspace.node_${kind}_hint`)}
                        </span>
                      </span>
                    </button>
                  ))}
                </div>
              </details>
            )}
            {!watching && (
              <button
                type="button"
                onClick={() => {
                  setInspector('settings');
                  openDock('node');
                }}
                className="inline-flex items-center gap-1.5 rounded-lg px-2 py-2 text-xs text-pc-text-muted hover:bg-pc-elevated"
              >
                <Settings2 className="h-3.5 w-3.5" />
                {t('sop_workspace.sop_settings')}
              </button>
            )}
            {!watching && (
              <button
                type="button"
                onClick={() => setSourceOpen(true)}
                className="rounded-lg px-2 py-2 text-xs text-pc-text-muted hover:bg-pc-elevated"
              >
                {t('sop_workspace.advanced_source')}
              </button>
            )}
            <button
              type="button"
              onClick={() => openDock('runs')}
              className="ml-auto inline-flex items-center gap-1.5 rounded-lg px-2 py-2 text-xs text-pc-text-muted hover:bg-pc-elevated"
            >
              <Activity className="h-3.5 w-3.5 text-pc-accent" />
              {t('sop_workspace.runs')}{' '}
              <span className="tabular-nums">{runs.filter((run) => run.active).length}</span>
            </button>
          </div>
          {editing === null && !watching && (
            <div className="mx-4 mb-3 flex flex-wrap items-center gap-3 rounded-xl border border-pc-accent/20 bg-pc-accent/5 p-4">
              <Bot className="h-5 w-5 shrink-0 text-pc-accent" />
              <div className="min-w-0 flex-1">
                <p className="text-sm font-medium">{t('sop_workspace.new_title')}</p>
                <p className="mt-1 text-xs text-pc-text-muted">{t('sop_workspace.new_hint')}</p>
              </div>
              <button
                type="button"
                onClick={() => {
                  setInspector('settings');
                  openDock('node');
                }}
                className="btn-secondary px-3 py-2 text-xs"
              >
                {t('sop_workspace.start_blank')}
              </button>
            </div>
          )}
          {(saveError ||
            (!watching && graphError) ||
            (watching && (runLoadError || overlayError))) && (
            <p role="alert" className="mx-4 mb-3 text-xs text-status-error">
              {saveError || (watching ? runLoadError || overlayError : graphError)}
            </p>
          )}
          {sourceDirty && (
            <button
              type="button"
              onClick={() => setSourceOpen(true)}
              className="mx-4 mb-3 text-left text-xs text-status-warning"
            >
              {t('workspace.apply_source_first')}
            </button>
          )}
          {watching && overlay && (
            <div className="mx-4 mb-3">
              <SopRunControls key={runId} overlay={overlay} onUpdate={setOverlay} />
            </div>
          )}
          <div
            inert={!watching && (sourceDirty || saving)}
            className="flex min-h-0 flex-1 flex-col px-4 pb-3"
          >
            {shownGraph && shownSop ? (
              <SopCanvas
                key={watching ? `run:${runId}` : 'edit'}
                draft={shownSop}
                graph={shownGraph}
                selectedStep={selectedStep}
                readOnly={watching}
                runStateByStep={watching ? runStates : undefined}
                fill
                onSelectStep={selectStep}
                onSelectTrigger={selectTrigger}
                onAddStep={() => {
                  if (palette.current) palette.current.open = true;
                }}
                onRemoveStep={(number) => {
                  const index = draft.steps.findIndex((step) => step.number === number);
                  if (index >= 0) handlers.onRemoveStep(index);
                }}
                onConnect={(from, to, kind, port) => editWire(from, to, kind, 'connect', port)}
                onDisconnect={(from, to, kind, port) =>
                  editWire(from, to, kind, 'disconnect', port)
                }
                onConnectData={(from, pin, to, input) =>
                  mutateDraft((sop) => writeStepBinding(sop, to, input, `{{steps.${from}.${pin}}}`))
                }
                onDisconnectData={(to, input) =>
                  mutateDraft((sop) => writeStepBinding(sop, to, input, null))
                }
                onMoveNode={(number, x, y) =>
                  mutateDraft((sop) => ({
                    ...sop,
                    steps: sop.steps.map((step) =>
                      step.number === number ? { ...step, pos: { x, y } } : step,
                    ),
                  }))
                }
                onUndo={undo}
                canUndo={undoStack.length > 0}
              />
            ) : (
              <div role="status" className="m-auto p-8 text-sm text-pc-text-muted">
                {t('common.loading')}
              </div>
            )}
            {!watching && draftGraph && draftGraph.diagnostics.length > 0 && (
              <div className="max-h-36 shrink-0 overflow-auto">
                <DiagnosticsPanel graph={draftGraph} />
              </div>
            )}
          </div>
          <footer className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-t border-pc-border px-4 py-2 text-[11px] text-pc-text-muted">
            <span>
              {t(watching ? 'sop_workspace.current_definition' : 'sop_workspace.canvas_hint')}
            </span>
            {!watching && (
              <button
                type="button"
                disabled={saving || sourceDirty || !dirty || assistantBusy}
                onClick={() => void reset()}
                className="underline disabled:opacity-40"
              >
                {t('sop_workspace.reset_draft')}
              </button>
            )}
          </footer>
        </section>
        {dockOpen && (
          <button
            type="button"
            aria-label={t('sop_workspace.close_panel')}
            className="absolute inset-0 z-20 bg-black/30 min-[1100px]:hidden"
            onClick={() => setDockOpen(false)}
          />
        )}
        <aside
          hidden={!dockOpen}
          aria-label={t('sop_workspace.panel')}
          className="absolute inset-y-0 right-0 z-30 flex w-full max-w-96 shrink-0 flex-col border-l border-pc-border bg-pc-surface min-[1100px]:static min-[1100px]:z-auto min-[1100px]:w-80 2xl:w-96 [&[hidden]]:hidden"
        >
          <div className="flex shrink-0 items-center border-b border-pc-border px-2">
            <div role="tablist" aria-label={t('sop_workspace.panel')} className="flex flex-1">
              {(['runs', 'node', 'agent'] as const).map((tab) => (
                <button
                  key={tab}
                  type="button"
                  role="tab"
                  id={`${panelId}-${tab}`}
                  aria-controls={`${panelId}-${tab}-panel`}
                  tabIndex={dockTab === tab ? 0 : -1}
                  aria-selected={dockTab === tab}
                  onKeyDown={(e) => {
                    const tabs = ['runs', 'node', 'agent'] as const;
                    const index = tabs.indexOf(tab);
                    const next =
                      e.key === 'ArrowRight'
                        ? tabs[(index + 1) % tabs.length]
                        : e.key === 'ArrowLeft'
                          ? tabs[(index + tabs.length - 1) % tabs.length]
                          : e.key === 'Home'
                            ? tabs[0]
                            : e.key === 'End'
                              ? tabs[2]
                              : undefined;
                    if (next) {
                      e.preventDefault();
                      openDock(next);
                      document.getElementById(`${panelId}-${next}`)?.focus();
                    }
                  }}
                  onClick={() => openDock(tab)}
                  className={`border-b-2 px-3 py-3 text-xs ${dockTab === tab ? 'border-pc-accent text-pc-text' : 'border-transparent text-pc-text-muted'}`}
                >
                  {t(`sop_workspace.${tab}`)}
                </button>
              ))}
            </div>
            <button
              type="button"
              onClick={() => setDockOpen(false)}
              aria-label={t('sop_workspace.close_panel')}
              className="rounded-md p-2 text-pc-text-muted hover:bg-pc-elevated"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
          <div
            role="tabpanel"
            id={`${panelId}-runs-panel`}
            aria-labelledby={`${panelId}-runs`}
            hidden={dockTab !== 'runs'}
            className="min-h-0 flex-1 overflow-auto"
          >
            {editing && (
              <div className="border-b border-pc-border p-4">
                <h2 className="mb-3 text-sm font-medium">{t('sop_workspace.start_run')}</h2>
                {dirty || sourceDirty ? (
                  <p className="mb-2 text-xs text-pc-text-muted">
                    {t('sop_workspace.save_before_run')}
                  </p>
                ) : null}
                <ManualRunPanel
                  name={editing}
                  sop={draft}
                  disabled={dirty || sourceDirty || saving}
                  onStarted={(id) => onSelectRun(editing, id)}
                />
                {!draft.triggers.some((trigger) => trigger.type === 'manual') && (
                  <p className="text-xs text-pc-text-muted">
                    {t('sop_workspace.automatic_trigger')}
                  </p>
                )}
              </div>
            )}
            <SopRunsPanel
              runs={runs}
              error={runError}
              name={editing}
              selectedRun={runId}
              onSelect={onSelectRun}
            />
          </div>
          <div
            role="tabpanel"
            id={`${panelId}-node-panel`}
            aria-labelledby={`${panelId}-node`}
            hidden={dockTab !== 'node'}
            className="min-h-0 flex-1 overflow-auto"
          >
            {watching ? (
              <SopRunInspector
                overlay={overlay}
                graph={runGraph}
                selectedStep={selectedStep}
                onSelect={setSelectedStep}
              />
            ) : (
              <div inert={sourceDirty || saving} className="space-y-4 p-4">
                <label className="block text-xs text-pc-text-muted">
                  {t('sop_workspace.select_node')}
                  <select
                    aria-label={t('sop_workspace.select_node')}
                    value={
                      inspector === 'settings'
                        ? 'settings'
                        : inspector === 'trigger'
                          ? `trigger:${selectedTrigger}`
                          : `step:${selectedStep}`
                    }
                    onChange={(e) => {
                      const [kind, value] = e.target.value.split(':');
                      if (kind === 'settings') setInspector('settings');
                      else if (kind === 'trigger') selectTrigger(Number(value));
                      else selectStep(Number(value));
                    }}
                    className="mt-2 w-full rounded-lg border border-pc-border bg-pc-surface p-2 text-sm text-pc-text"
                  >
                    <option value="settings">{t('sop_workspace.sop_settings')}</option>
                    {draft.triggers.map((trigger, index) => (
                      <option key={`trigger:${index}`} value={`trigger:${index}`}>
                        {t('sops.triggers')} · {triggerSource(trigger)}
                      </option>
                    ))}
                    {draft.steps.map((step) => (
                      <option key={`step:${step.number}`} value={`step:${step.number}`}>
                        {step.number}. {step.title || t('sops.step')}
                      </option>
                    ))}
                  </select>
                </label>
                {inspector === 'settings' && (
                  <DraftSidebar
                    draft={draft}
                    selectedStep={selectedStep}
                    selectedTrigger={selectedTrigger}
                    triggerRegistry={triggerRegistry}
                    agentAliases={agentAliases}
                    decisionModelOptions={decisionModelOptions}
                    onSelectStep={selectStep}
                    {...handlers}
                    onAddStep={() => addNode('agent')}
                  />
                )}
                {inspector === 'step' && (
                  <StepInspector
                    draft={draft}
                    selectedStep={selectedStep}
                    runCallsByStep={capturedCalls}
                    agentAliases={agentAliases}
                    onStep={handlers.onStep}
                    onRemoveStep={handlers.onRemoveStep}
                    onMoveStep={handlers.onMoveStep}
                  />
                )}
                {inspector === 'trigger' && currentTrigger && selectedTrigger !== null && (
                  <TriggerEditor
                    trigger={currentTrigger}
                    index={selectedTrigger}
                    selected
                    registry={triggerRegistry}
                    onChange={(value) => handlers.onTrigger(selectedTrigger, value)}
                    onRemove={() => {
                      handlers.onRemoveTrigger(selectedTrigger);
                      setInspector('settings');
                    }}
                  />
                )}
              </div>
            )}
          </div>
          <div
            role="tabpanel"
            id={`${panelId}-agent-panel`}
            aria-labelledby={`${panelId}-agent`}
            hidden={dockTab !== 'agent'}
            className="min-h-0 flex-1 overflow-hidden"
          >
            {assistantOpened && (
              <Code
                embedded
                contextText={JSON.stringify(draft, null, 2)}
                sopAssistant={{ onApply: applyProposal }}
                onBusyChange={setAssistantBusy}
                onAttentionOpen={() => openDock('agent')}
                attentionTarget={editing ? `/sops/${encodeURIComponent(editing)}` : '/sops/new'}
              />
            )}
          </div>
        </aside>
      </div>
      <dialog
        ref={sourceDialog}
        onClose={() => setSourceOpen(false)}
        aria-label={t('sop_workspace.advanced_source')}
        className="m-auto h-[80dvh] w-[calc(100%-2rem)] max-w-4xl rounded-xl border border-pc-border bg-pc-surface p-0 text-pc-text shadow-2xl backdrop:bg-black/50"
      >
        <div className="flex h-full flex-col">
          <div className="flex items-center justify-between border-b border-pc-border px-4 py-3">
            <h2 className="text-sm font-medium">{t('sop_workspace.advanced_source')}</h2>
            <button
              type="button"
              onClick={() => setSourceOpen(false)}
              aria-label={t('common.close')}
              className="rounded p-2 hover:bg-pc-elevated"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
          <SopSourceEditor
            draft={draft}
            storageKey={`${draftStorageKey(editing)}:source`}
            onDirty={setSourceDirty}
            onApply={(next) =>
              commitDraft({
                ...next,
                steps: next.steps.map((step) => ({
                  ...blankStep(step.number),
                  ...step,
                })),
              })
            }
          />
        </div>
      </dialog>
    </div>
  );
}
