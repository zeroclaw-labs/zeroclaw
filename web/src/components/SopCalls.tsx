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

import { useState, type ReactNode } from 'react';
import { ChevronDown, ChevronRight, Pin, Plus, Trash2 } from 'lucide-react';
import { t } from '@/lib/i18n';
import type { PlannedToolCall, StepToolCall } from '@/lib/sops';

const INPUT_CLS = 'w-full rounded border border-pc-border bg-pc-surface px-2 py-1 text-pc-text';

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

export function PlannedCallsEditor({
  calls,
  captured,
  onChange,
}: {
  calls: PlannedToolCall[];
  /// Captured calls for the same step from a watched run, used to pin
  /// sample outputs onto planned calls at the same index.
  captured?: StepToolCall[];
  onChange: (next: PlannedToolCall[]) => void;
}) {
  const [openIdx, setOpenIdx] = useState<number | null>(null);
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
            return (
              <Accordion
                key={i}
                open={openIdx === i}
                onToggle={() => setOpenIdx((cur) => (cur === i ? null : i))}
                header={
                  <>
                    <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded bg-pc-accent-light text-[10px] font-semibold text-pc-accent">
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
                  <label className="block flex-1 text-xs">
                    <span className="mb-1 block text-pc-text-muted">{t('sops.call_tool')}</span>
                    <input
                      type="text"
                      value={call.tool}
                      onChange={(e) => setCall(i, { tool: e.target.value })}
                      placeholder="calculator"
                      className={`${INPUT_CLS} font-mono text-xs`}
                    />
                  </label>
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
                <JsonField
                  label={t('sops.call_args')}
                  value={call.args}
                  onChange={(next) => setCall(i, { args: next })}
                  placeholder={'{"function": "add", "values": "{{steps.1.value}}"}'}
                  rows={4}
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
              <span className="inline-flex h-4 w-4 shrink-0 items-center justify-center rounded bg-pc-accent-light text-[10px] font-semibold text-pc-accent">
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
