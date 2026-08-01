// Planned/captured tool-call panels for the SOP authoring surfaces.
//
// `PlannedCallsEditor` is the per-call accordion used inside the step
// editor: each planned call folds out to a tool name, an args template
// (JSON, may embed `{{steps.N.path}}` / `{{calls.K.path}}` bindings),
// and an optional pinned sample output. When a captured run call is
// available at the same index its output can be pinned in one click.
//
// `CapturedCallList` is the read-only accordion shown in the step
// inspector while watching a run: per call it shows args, display
// output, and structured output data.

import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { ChevronDown, ChevronRight, Pin, Plus, Trash2 } from 'lucide-react';
import { t } from '@/lib/i18n';
import type { PlannedToolCall, StepToolCall } from '@/lib/sops';
import { loadCatalog, type CatalogEntry } from '@/components/ToolPicker';

const INPUT_CLS = 'w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text';

/// Shared, cached load of the tool catalog (built-in agent tools + CLI tools).
/// `loadCatalog` is process-cached, so every mounted editor resolves instantly
/// after the first fetch.
function useToolCatalog(agent?: string | null): CatalogEntry[] | null {
  const [catalog, setCatalog] = useState<CatalogEntry[] | null>(null);
  useEffect(() => {
    let live = true;
    loadCatalog(agent ?? undefined)
      .then((entries) => {
        if (live) setCatalog(entries);
      })
      .catch(() => {
        if (live) setCatalog([]);
      });
    return () => {
      live = false;
    };
  }, [agent]);
  return catalog;
}

function stringify(value: unknown): string {
  return JSON.stringify(value ?? {}, null, 2);
}

/// JSON textarea that keeps invalid intermediate text local and only
/// propagates parseable values. The parse error stays visible until fixed.
function JsonField({
  label,
  value,
  onChange,
  placeholder,
  rows = 3,
}: {
  label: string;
  value: unknown;
  onChange: (next: unknown) => void;
  placeholder?: string;
  rows?: number;
}) {
  const [text, setText] = useState<string | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const shown = text ?? stringify(value);
  return (
    <label className="block text-xs">
      <span className="mb-1 block text-pc-text-muted">{label}</span>
      <textarea
        value={shown}
        rows={rows}
        placeholder={placeholder}
        spellCheck={false}
        onChange={(e) => {
          const raw = e.target.value;
          setText(raw);
          try {
            onChange(JSON.parse(raw));
            setParseError(null);
          } catch (err) {
            setParseError(err instanceof Error ? err.message : String(err));
          }
        }}
        onBlur={() => {
          if (!parseError) setText(null);
        }}
        className={`${INPUT_CLS} font-mono text-xs`}
      />
      {parseError ? <p className="mt-1 text-xs text-status-error">{parseError}</p> : null}
    </label>
  );
}

function Accordion({
  header,
  open,
  onToggle,
  children,
}: {
  header: ReactNode;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div className="rounded border border-pc-border">
      <button
        type="button"
        onClick={onToggle}
        className="flex w-full items-center gap-1.5 px-2 py-1.5 text-left text-xs text-pc-text hover:bg-pc-elevated"
      >
        {open ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-pc-text-muted" aria-hidden />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-pc-text-muted" aria-hidden />
        )}
        {header}
      </button>
      {open ? <div className="space-y-2 border-t border-pc-border p-2">{children}</div> : null}
    </div>
  );
}

/// Single-select over the same tool catalog the scope `ToolPicker` walks
/// (built-in agent tools + discovered CLI tools). A planned call's tool is a
/// registry name, never free text. A value not in the catalog (removed tool,
/// or an MCP tool absent from the default-agent listing) is preserved as its
/// own option so editing never silently drops it.
function ToolSelect({
  value,
  catalog,
  onChange,
}: {
  value: string;
  catalog: CatalogEntry[] | null;
  onChange: (next: string) => void;
}) {
  const agent = (catalog ?? []).filter((e) => e.group === 'agent');
  const cli = (catalog ?? []).filter((e) => e.group === 'cli');
  const known = (catalog ?? []).some((e) => e.name === value);

  return (
    <label className="block flex-1 text-xs">
      <span className="mb-1 block text-pc-text-muted">{t('sops.call_tool')}</span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={`${INPUT_CLS} font-mono text-xs`}
      >
        <option value="" disabled>
          {t('sops.call_untitled')}
        </option>
        {value && !known ? <option value={value}>{value}</option> : null}
        {agent.length > 0 ? (
          <optgroup label={t('tool_picker.group_agent')}>
            {agent.map((e) => (
              <option key={e.name} value={e.name}>
                {e.name}
              </option>
            ))}
          </optgroup>
        ) : null}
        {cli.length > 0 ? (
          <optgroup label={t('tool_picker.group_cli')}>
            {cli.map((e) => (
              <option key={e.name} value={e.name}>
                {e.name}
              </option>
            ))}
          </optgroup>
        ) : null}
      </select>
    </label>
  );
}

// ── Schema-driven args editor ────────────────────────────────────────────────
// A planned call's args are shaped by the selected tool's JSON Schema, so the
// editor renders one typed field per schema property instead of a raw JSON
// blob. Any field may instead hold a `{{steps.N.path}}` / `{{calls.K.path}}`
// binding: a whole-string binding is passed through verbatim and keeps its
// runtime type. Tools with no usable object schema (unknown tool, or a schema
// without `properties`) fall back to the JSON textarea so nothing is lost.

interface SchemaProp {
  type?: string | string[];
  description?: string;
  enum?: unknown[];
}

interface ObjectSchema {
  properties: Record<string, SchemaProp>;
  required: string[];
}

function objectSchema(parameters: unknown): ObjectSchema | null {
  if (!parameters || typeof parameters !== 'object') return null;
  const schema = parameters as Record<string, unknown>;
  const props = schema.properties;
  if (!props || typeof props !== 'object') return null;
  const required = Array.isArray(schema.required)
    ? (schema.required.filter((r) => typeof r === 'string') as string[])
    : [];
  return { properties: props as Record<string, SchemaProp>, required };
}

function primaryType(prop: SchemaProp): string {
  if (Array.isArray(prop.type)) {
    return prop.type.find((x) => x !== 'null') ?? 'string';
  }
  return prop.type ?? 'string';
}

function isBinding(value: unknown): value is string {
  return typeof value === 'string' && value.includes('{{');
}

/// One schema property → one control. String/number/boolean/enum get native
/// inputs; array/object properties fall back to a per-key JSON field. A value
/// holding a `{{…}}` binding always renders as a text input so the binding is
/// editable regardless of the declared type.
function SchemaField({
  name,
  prop,
  required,
  value,
  onChange,
}: {
  name: string;
  prop: SchemaProp;
  required: boolean;
  value: unknown;
  onChange: (next: unknown) => void;
}) {
  const type = primaryType(prop);
  const label = (
    <span className="mb-1 block text-pc-text-muted">
      <span className="font-mono">{name}</span>
      {required ? <span className="text-status-error"> *</span> : null}
      {prop.description ? (
        <span className="ml-1 text-pc-text-faint">{prop.description}</span>
      ) : null}
    </span>
  );

  if (isBinding(value)) {
    return (
      <label className="block text-xs">
        {label}
        <input
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className={`${INPUT_CLS} font-mono text-xs`}
        />
      </label>
    );
  }

  if (Array.isArray(prop.enum) && prop.enum.length > 0) {
    return (
      <label className="block text-xs">
        {label}
        <select
          value={value === undefined || value === null ? '' : String(value)}
          onChange={(e) => onChange(e.target.value === '' ? undefined : e.target.value)}
          className={`${INPUT_CLS} font-mono text-xs`}
        >
          <option value="">{t('sops.arg_unset')}</option>
          {prop.enum.map((opt) => (
            <option key={String(opt)} value={String(opt)}>
              {String(opt)}
            </option>
          ))}
        </select>
      </label>
    );
  }

  if (type === 'boolean') {
    return (
      <label className="flex items-center gap-2 text-xs">
        <input
          type="checkbox"
          checked={value === true}
          onChange={(e) => onChange(e.target.checked)}
        />
        {label}
      </label>
    );
  }

  if (type === 'number' || type === 'integer') {
    return (
      <label className="block text-xs">
        {label}
        <input
          type="text"
          inputMode="decimal"
          value={value === undefined || value === null ? '' : String(value)}
          placeholder={t('sops.arg_binding_placeholder')}
          onChange={(e) => {
            const raw = e.target.value.trim();
            if (raw === '') {
              onChange(undefined);
            } else if (raw.includes('{{')) {
              onChange(raw);
            } else {
              const num = Number(raw);
              onChange(Number.isNaN(num) ? raw : num);
            }
          }}
          className={`${INPUT_CLS} font-mono text-xs`}
        />
      </label>
    );
  }

  if (type === 'array' || type === 'object') {
    return (
      <JsonField
        label={name + (required ? ' *' : '')}
        value={value ?? (type === 'array' ? [] : {})}
        onChange={onChange}
        placeholder={t('sops.arg_binding_placeholder')}
        rows={3}
      />
    );
  }

  // string and anything else
  return (
    <label className="block text-xs">
      {label}
      <input
        type="text"
        value={value === undefined || value === null ? '' : String(value)}
        placeholder={t('sops.arg_binding_placeholder')}
        onChange={(e) => onChange(e.target.value === '' ? undefined : e.target.value)}
        className={`${INPUT_CLS} font-mono text-xs`}
      />
    </label>
  );
}

function SchemaArgsEditor({
  parameters,
  args,
  onChange,
}: {
  parameters: unknown;
  args: unknown;
  onChange: (next: unknown) => void;
}) {
  const schema = useMemo(() => objectSchema(parameters), [parameters]);
  const current = (args && typeof args === 'object' && !Array.isArray(args)
    ? args
    : {}) as Record<string, unknown>;

  if (!schema) {
    return (
      <JsonField
        label={t('sops.call_args')}
        value={args}
        onChange={onChange}
        placeholder={'{"function": "add", "values": "{{steps.1.value}}"}'}
        rows={4}
      />
    );
  }

  const setField = (name: string, next: unknown) => {
    const merged = { ...current };
    if (next === undefined) {
      delete merged[name];
    } else {
      merged[name] = next;
    }
    onChange(merged);
  };

  const names = Object.keys(schema.properties);
  if (names.length === 0) {
    return <div className="text-xs text-pc-text-faint">{t('sops.arg_none')}</div>;
  }

  return (
    <div className="space-y-2">
      <span className="block text-xs text-pc-text-muted">{t('sops.call_args')}</span>
      {names.map((name) => {
        const prop = schema.properties[name];
        if (!prop) return null;
        return (
          <SchemaField
            key={name}
            name={name}
            prop={prop}
            required={schema.required.includes(name)}
            value={current[name]}
            onChange={(next) => setField(name, next)}
          />
        );
      })}
    </div>
  );
}

export function PlannedCallsEditor({
  calls,
  captured,
  agent,
  onChange,
}: {
  calls: PlannedToolCall[];
  /// Captured calls for the same step from a watched run, used to pin
  /// sample outputs onto planned calls at the same index.
  captured?: StepToolCall[];
  /// Effective agent for the owning step (step-level override or the SOP's
  /// agent), so the tool catalog scopes to that agent's real tool set.
  agent?: string | null;
  onChange: (next: PlannedToolCall[]) => void;
}) {
  const [openIdx, setOpenIdx] = useState<number | null>(null);
  const catalog = useToolCatalog(agent);
  const setCall = (i: number, patch: Partial<PlannedToolCall>) => {
    onChange(calls.map((c, j) => (j === i ? { ...c, ...patch } : c)));
  };
  return (
    <div className="rounded border border-pc-border p-2">
      <div className="mb-1 flex items-center justify-between">
        <span className="text-xs font-medium text-pc-text">{t('sops.planned_calls')}</span>
        <button
          type="button"
          onClick={() => {
            onChange([...calls, { tool: '', args: {} }]);
            setOpenIdx(calls.length);
          }}
          className="rounded border border-pc-border px-2 py-0.5 text-xs text-pc-text hover:bg-pc-elevated"
        >
          <Plus className="mr-1 inline h-3 w-3" aria-hidden />
          {t('sops.add_call')}
        </button>
      </div>
      {calls.length === 0 ? (
        <div className="text-xs text-pc-text-faint">{t('sops.no_calls')}</div>
      ) : (
        <div className="space-y-1">
          {calls.map((call, i) => {
            const sample = captured?.find((c) => c.index === i && c.tool === call.tool);
            const schemaParams = catalog?.find((e) => e.name === call.tool)?.parameters;
            return (
              <Accordion
                key={i}
                open={openIdx === i}
                onToggle={() => setOpenIdx((cur) => (cur === i ? null : i))}
                header={
                  <>
                    <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded bg-pc-accent text-[10px] font-semibold text-[#0b1220]">
                      {i}
                    </span>
                    <span className="min-w-0 flex-1 truncate font-mono">
                      {call.tool || t('sops.call_untitled')}
                    </span>
                    {call.pinned !== undefined && call.pinned !== null ? (
                      <Pin className="h-3 w-3 shrink-0 text-pc-accent" aria-hidden />
                    ) : null}
                  </>
                }
              >
                <div className="flex items-end gap-2">
                  <ToolSelect
                    value={call.tool}
                    catalog={catalog}
                    onChange={(next) => setCall(i, { tool: next })}
                  />
                  <button
                    type="button"
                    onClick={() => {
                      onChange(calls.filter((_, j) => j !== i));
                      setOpenIdx(null);
                    }}
                    className="rounded px-1.5 py-1 text-status-error hover:bg-pc-elevated"
                    aria-label={t('sops.remove_call')}
                  >
                    <Trash2 className="h-3.5 w-3.5" aria-hidden />
                  </button>
                </div>
                <SchemaArgsEditor
                  parameters={schemaParams}
                  args={call.args}
                  onChange={(next) => setCall(i, { args: next })}
                />
                <p className="text-xs text-pc-text-faint">{t('sops.call_binding_hint')}</p>
                <div className="flex items-center justify-between">
                  <span className="text-xs text-pc-text-muted">{t('sops.call_pinned')}</span>
                  <div className="flex gap-2">
                    {sample?.output_data !== undefined && sample?.output_data !== null ? (
                      <button
                        type="button"
                        onClick={() => setCall(i, { pinned: sample.output_data })}
                        className="rounded border border-pc-border px-2 py-0.5 text-xs text-pc-text hover:bg-pc-elevated"
                      >
                        <Pin className="mr-1 inline h-3 w-3" aria-hidden />
                        {t('sops.pin_from_run')}
                      </button>
                    ) : null}
                    {call.pinned !== undefined && call.pinned !== null ? (
                      <button
                        type="button"
                        onClick={() => setCall(i, { pinned: null })}
                        className="rounded border border-pc-border px-2 py-0.5 text-xs text-pc-text-muted hover:bg-pc-elevated"
                      >
                        {t('sops.unpin')}
                      </button>
                    ) : null}
                  </div>
                </div>
                {call.pinned !== undefined && call.pinned !== null ? (
                  <JsonField
                    label={t('sops.call_pinned_output')}
                    value={call.pinned}
                    onChange={(next) => setCall(i, { pinned: next })}
                    rows={3}
                  />
                ) : null}
              </Accordion>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function CapturedCallList({ calls }: { calls: StepToolCall[] }) {
  const [openIdx, setOpenIdx] = useState<number | null>(null);
  if (calls.length === 0) return null;
  return (
    <div className="space-y-1">
      {calls.map((call) => (
        <Accordion
          key={call.index}
          open={openIdx === call.index}
          onToggle={() => setOpenIdx((cur) => (cur === call.index ? null : call.index))}
          header={
            <>
              <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded bg-pc-accent text-[10px] font-semibold text-[#0b1220]">
                {call.index}
              </span>
              <span className="min-w-0 flex-1 truncate font-mono">{call.tool}</span>
              <span
                className={`shrink-0 text-[10px] ${call.success ? 'text-status-success' : 'text-status-error'}`}
              >
                {call.success ? t('sops.call_ok') : t('sops.call_failed')}
              </span>
              <span className="shrink-0 text-[10px] text-pc-text-faint">{call.duration_ms}ms</span>
            </>
          }
        >
          <div className="text-xs">
            <span className="mb-1 block text-pc-text-muted">{t('sops.call_args')}</span>
            <pre className="max-h-40 overflow-auto rounded bg-pc-bg-base p-2 font-mono text-xs text-pc-text">
              {stringify(call.args)}
            </pre>
          </div>
          {call.error ? (
            <div className="text-xs text-status-error">{call.error}</div>
          ) : null}
          <div className="text-xs">
            <span className="mb-1 block text-pc-text-muted">{t('sops.call_output')}</span>
            <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded bg-pc-bg-base p-2 font-mono text-xs text-pc-text">
              {call.output}
            </pre>
          </div>
          {call.output_data !== undefined && call.output_data !== null ? (
            <div className="text-xs">
              <span className="mb-1 block text-pc-text-muted">{t('sops.call_output_data')}</span>
              <pre className="max-h-40 overflow-auto rounded bg-pc-bg-base p-2 font-mono text-xs text-pc-text">
                {stringify(call.output_data)}
              </pre>
            </div>
          ) : null}
        </Accordion>
      ))}
    </div>
  );
}
