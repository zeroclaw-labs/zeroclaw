// Shared form renderer for a section's fields. Used by both /onboard and
// /config. Walks the entries returned by GET /api/config/list?prefix=...,
// dispatches each input by `kind` (no value-sniffing), and submits all
// changed fields as one PATCH on save.
//
// Per-field behavior:
//  * bool       → <select> true/false
//  * enum       → <select> with enum_variants
//  * string-array → <textarea>, one value per line
//  * integer/float → <input type="number">
//  * secret     → <input type="password"> with populated indicator
//  * provider model field (path matches `providers.models.<name>.model`) →
//    fetches /api/onboard/catalog/models?provider=<name>, populates a
//    <datalist>; on fetch failure falls back to free-text with help text.
//  * everything else → <input type="text">
//
// Each field carries an optional comment input (per-PATCH-op `comment`).
//
// On error: structured ApiError envelope binds inline to the field by .path.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from 'react';
import { List as ListIcon, Plus, Save, Trash2, Type as TypeIcon } from 'lucide-react';
import {
  ApiError,
  deleteProp,
  descriptionForPath,
  fetchConfigSchema,
  getCatalogModels,
  listProps,
  objectArrayElementProps,
  patchConfig,
  type ConfigApiError,
  type DriftEntry,
  type ListResponseEntry,
  type ObjectArrayPropMeta,
  type PatchOp,
  type ValidationWarning,
} from '../../lib/api';
import { fuzzyFilter } from '../../lib/fuzzy';
import { t, tConfigDescription, tConfigLabel, tConfigPlaceholder } from '../../lib/i18n';

interface FieldFormProps {
  /** Dotted prefix to fetch fields under, e.g. `providers.models.anthropic`. */
  prefix: string;
  /** Called after a successful save; parent typically advances or refreshes. */
  onSaved?: () => void;
  /** Hide the trash icon (per-prop reset) when the parent doesn't want it. */
  showDelete?: boolean;
  /** Optional title rendered above the form. */
  title?: string;
  /** Drift entries from the page-level fetch — passed through so each
   *  drifted field renders an inline `in-memory: [...] / on-disk: [...]`
   *  comparison next to its label. Empty / undefined when nothing drifted. */
  drift?: DriftEntry[];
}

/** Imperative handle the parent uses to flush unsaved changes before
 *  advancing the wizard. Resolves `true` when the form was clean or the
 *  save succeeded; `false` if the save failed (so the parent can stop). */
export interface FieldFormHandle {
  flushSave: () => Promise<boolean>;
}

function rendererFor(
  entry: ListResponseEntry,
): 'bool' | 'array' | 'object-array' | 'secret' | 'select' | 'number' | 'text' {
  if (entry.is_secret) return 'secret';
  switch (entry.kind) {
    case 'bool':
      return 'bool';
    case 'string-array':
      return 'array';
    case 'object-array':
      return 'object-array';
    case 'integer':
    case 'float':
      return 'number';
    case 'enum':
      return entry.enum_variants && entry.enum_variants.length > 0 ? 'select' : 'text';
    default:
      return 'text';
  }
}

function fieldShortLabel(entry: ListResponseEntry): string {
  return entry.path.split('.').pop()!.replace(/[-_]/g, ' ');
}

function formatMessage(template: string, values: Record<string, string | number>): string {
  return Object.entries(values).reduce(
    (out, [key, value]) => out.split(`{${key}}`).join(String(value)),
    template,
  );
}

function defaultInputValue(entry: ListResponseEntry): string {
  const v = entry.value;
  if (entry.kind === 'string-array' || entry.kind === 'object-array') {
    // API returns the TOML/JSON array form as a string. Keep it as the
    // canonical draft shape; the row editor parses on render.
    if (typeof v === 'string') return v === '<unset>' ? '[]' : v;
    if (Array.isArray(v)) return JSON.stringify(v);
    return '[]';
  }
  if (typeof v === 'string') return v === '<unset>' ? '' : v;
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  if (Array.isArray(v)) return v.join('\n');
  return '';
}

function parseInput(entry: ListResponseEntry, raw: string): unknown {
  switch (rendererFor(entry)) {
    case 'bool':
      return raw === 'true';
    case 'array':
      return parseArrayDraft(raw);
    case 'object-array': {
      const trimmed = raw.trim();
      if (!trimmed) return [];
      try {
        const parsed = JSON.parse(trimmed);
        return Array.isArray(parsed) ? parsed : [];
      } catch {
        return [];
      }
    }
    case 'number': {
      const n = Number(raw);
      return Number.isNaN(n) ? raw : n;
    }
    default:
      return raw;
  }
}

// Parse the draft string for a Vec<String> field. Accepts the JSON-array
// form (the canonical shape both the chip editor and the textarea view
// emit), with comma- / newline-separated as a fallback for hand-typed
// freeform input. Trims whitespace and drops empty entries on save.
function parseArrayDraft(raw: string): string[] {
  const trimmed = raw.trim();
  if (!trimmed) return [];
  if (trimmed.startsWith('[')) {
    try {
      const parsed = JSON.parse(trimmed);
      if (Array.isArray(parsed)) {
        return parsed
          .map((v) => String(v))
          .map((s) => s.trim())
          .filter((s) => s.length > 0);
      }
    } catch {
      /* fall through to freeform split */
    }
  }
  return raw
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

function parseArrayRows(value: string): string[] {
  if (!value) return [];
  try {
    const parsed = JSON.parse(value);
    if (Array.isArray(parsed)) return parsed.map((v) => String(v));
  } catch {
    // Fallback: comma- or newline-separated freeform.
    return value
      .split(/[\n,]/)
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }
  return [];
}

// `Option<Vec<String>>` carries a three-state distinction: None / [] / ["a"].
// Detected via type_hint so the chip editor can offer a separate "Clear (set
// to none)" affordance and the save path can emit `null` for empty + optional.
function isOptionalArray(typeHint: string): boolean {
  const compact = typeHint.replace(/\s+/g, '');
  return compact.startsWith('Option<Vec<') || compact.startsWith('Option<HashSet<');
}

const modelsCache: Record<string, { models: string[]; live: boolean }> = {};

const FieldForm = forwardRef<FieldFormHandle, FieldFormProps>(function FieldForm(
  { prefix, onSaved, showDelete = true, title, drift },
  ref,
) {
  const [entries, setEntries] = useState<ListResponseEntry[]>([]);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [comments, setComments] = useState<Record<string, string>>({});
  const [fieldErrors, setFieldErrors] = useState<Record<string, ConfigApiError>>({});
  const [topError, setTopError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<string | null>(null);
  // Non-fatal validation warnings echoed by the gateway after save.
  // Surfaced inline so dashboard users see what CLI users see on stderr —
  // e.g. `providers.fallback` referencing a non-existent provider returns
  // 200 (the value saved) plus a warning the user needs to address.
  const [warnings, setWarnings] = useState<ValidationWarning[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [schema, setSchema] = useState<Record<string, unknown> | undefined>(undefined);
  const [filter, setFilter] = useState('');

  // Schema is whole-Config and ETag-cached server-side; fetch once per
  // session so every form row can resolve its `///` doc-comment helper
  // text via descriptionForPath without per-field round trips.
  useEffect(() => {
    let cancelled = false;
    void fetchConfigSchema().then((s) => {
      if (!cancelled) setSchema(s);
    });
    return () => { cancelled = true; };
  }, []);

  const reload = async () => {
    setLoading(true);
    setTopError(null);
    try {
      const resp = await listProps(prefix);
      setEntries(resp.entries);
      const seed: Record<string, string> = {};
      for (const e of resp.entries) seed[e.path] = defaultInputValue(e);
      setDraft(seed);
    } catch (e) {
      if (e instanceof ApiError) {
        setTopError(`[${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setTopError(`Couldn't load fields for ${prefix}: ${e instanceof Error ? e.message : String(e)}`);
      }
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prefix]);

  // Returns true when nothing was dirty or the save succeeded; false on
  // any error so callers (e.g. the wizard's Next button) can refuse to
  // advance past a broken state.
  const handleSave = async (): Promise<boolean> => {
    setSaving(true);
    setSavedAt(null);
    setTopError(null);
    setFieldErrors({});
    setWarnings([]);

    const ops: PatchOp[] = [];
    for (const e of entries) {
      const raw = draft[e.path] ?? '';
      const original = defaultInputValue(e);
      // Secrets with empty input mean "don't change".
      if (e.is_secret && raw.length === 0) continue;
      if (raw === original) continue;
      let value: unknown = parseInput(e, raw);
      // For Option<Vec<String>>: empty rows = "no opinion" → send null
      // (clears the field). Mandatory Vec<String>: empty stays as [] (an
      // explicitly empty list, distinct from None).
      if (
        e.kind === 'string-array'
        && Array.isArray(value)
        && value.length === 0
        && isOptionalArray(e.type_hint)
      ) {
        value = null;
      }
      const op: PatchOp = { op: 'replace', path: e.path, value };
      const c = comments[e.path];
      if (c && c.length > 0) op.comment = c;
      ops.push(op);
    }

    if (ops.length === 0) {
      setSaving(false);
      return true;
    }

    try {
      const resp = await patchConfig(ops);
      setSavedAt(formatMessage(t('config.fields.saved'), { count: resp.results.length }));
      setWarnings(resp.warnings ?? []);
      await reload();
      onSaved?.();
      return true;
    } catch (e) {
      if (e instanceof ApiError) {
        const env = e.envelope as ConfigApiError;
        if (env.path) {
          setFieldErrors({ [env.path]: env });
          setTopError(`Save failed: [${env.code}] ${env.message} (field: ${env.path})`);
        } else {
          setTopError(`Save failed: [${env.code}] ${env.message}`);
        }
      } else {
        setTopError(`Save failed: ${e instanceof Error ? e.message : String(e)}`);
      }
      return false;
    } finally {
      setSaving(false);
    }
  };

  useImperativeHandle(ref, () => ({
    flushSave: handleSave,
  }));

  const handleDelete = async (path: string) => {
    try {
      await deleteProp(path);
      await reload();
    } catch (e) {
      if (e instanceof ApiError) {
        setTopError(`Delete failed: [${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setTopError(`Delete failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  };

  const sortedEntries = useMemo(() => {
    // Stable order: `enabled` first (drives whether anything below it
    // matters), then secrets (most-needed), then alphabetical by short
    // label. Curating `enabled` is safe — it's a load-bearing standard
    // field name across every section that has on/off semantics.
    const isEnabledLeaf = (e: ListResponseEntry) => e.path.endsWith('.enabled') || e.path === 'enabled';
    return [...entries].sort((a, b) => {
      const ea = isEnabledLeaf(a);
      const eb = isEnabledLeaf(b);
      if (ea !== eb) return ea ? -1 : 1;
      if (a.is_secret !== b.is_secret) return a.is_secret ? -1 : 1;
      return fieldShortLabel(a).localeCompare(fieldShortLabel(b));
    });
  }, [entries]);

  // Fuzzy-filter against the short label + dotted path; sections with
  // a lot of fields (Agent, Channels) are otherwise a wall of inputs.
  // Empty query falls through to the full sorted list. Matches the
  // pattern SectionPicker uses so behavior is consistent across views.
  const visibleEntries = useMemo(() => {
    if (!filter.trim()) return sortedEntries;
    return fuzzyFilter(sortedEntries, filter, (e) => `${fieldShortLabel(e)} ${e.path}`);
  }, [sortedEntries, filter]);

  // Count of fields whose draft value differs from the saved display value.
  // Drives the unsaved-changes counter in the sticky save bar. Must be
  // declared above the conditional render so hook count stays stable
  // across the loading / loaded transition (React error #310).
  const unsavedCount = useMemo(() => {
    let n = 0;
    for (const e of entries) {
      const raw = draft[e.path] ?? '';
      const original = defaultInputValue(e);
      if (e.is_secret && raw.length === 0) continue;
      if (raw !== original) n += 1;
    }
    return n;
  }, [entries, draft]);

  // Warn user before navigating away with unsaved changes.
  useEffect(() => {
    if (unsavedCount === 0) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', handler);
    return () => window.removeEventListener('beforeunload', handler);
  }, [unsavedCount]);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4 pb-20">
      {/* pb-20 reserves space at the bottom so the last field isn't covered
          by the sticky save bar when the form is short. */}
      {title && (
        <h2
          className="text-lg font-semibold"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          {title}
        </h2>
      )}

      {entries.length > 4 && (
        <input
          type="text"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder={formatMessage(t('config.fields.filter_placeholder'), { count: entries.length })}
          className="input-electric w-full px-3 py-2 text-sm"
          aria-label={t('config.fields.filter_aria')}
        />
      )}

      {entries.length === 0 ? (
        <div
          className="surface-panel p-6 text-center text-sm"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          {formatMessage(t('config.fields.empty_under'), { prefix })}
          <code style={{ color: 'var(--pc-text-faint)' }}>{prefix}</code>.
        </div>
      ) : (
        <form
          className="surface-panel divide-y"
          style={{ borderColor: 'var(--pc-border)' }}
          onSubmit={(e) => {
            e.preventDefault();
            void handleSave().catch(() => undefined);
          }}
        >
          {visibleEntries.length === 0 ? (
            <div
              className="px-4 py-6 text-sm text-center"
              style={{ color: 'var(--pc-text-muted)' }}
            >
              {formatMessage(t('config.fields.no_match'), { filter })}
              <code style={{ color: 'var(--pc-text-faint)' }}>{filter}</code>.
            </div>
          ) : null}
          {visibleEntries.map((f) => (
            <FieldRow
              key={f.path}
              entry={f}
              value={draft[f.path] ?? ''}
              onChange={(v) => setDraft((d) => ({ ...d, [f.path]: v }))}
              comment={comments[f.path] ?? ''}
              onCommentChange={(v) => setComments((c) => ({ ...c, [f.path]: v }))}
              error={fieldErrors[f.path]}
              onDelete={showDelete ? () => handleDelete(f.path) : undefined}
              description={tConfigDescription(f.path, descriptionForPath(schema, f.path))}
              elementProps={
                f.kind === 'object-array' ? objectArrayElementProps(schema, f.path) : null
              }
              drift={drift?.find((d) => d.path === f.path) ?? null}
            />
          ))}
        </form>
      )}

      {/* Sticky footer bar — pinned to the bottom of the scrolling form
          area so Save is always visible without scrolling. Status (unsaved
          count / save success / save error) renders inline next to the
          button so post-save feedback lands where the eye already is. */}
      {entries.length > 0 && (
        <div
          className="sticky bottom-0 left-0 right-0 -mx-6 px-6 py-3 border-t backdrop-blur z-10"
          style={{
            borderColor: 'var(--pc-border)',
            background: 'color-mix(in srgb, var(--pc-bg-base) 88%, transparent)',
          }}
        >
          {/* Warnings echoed by the gateway after a successful save —
              e.g. `providers.fallback` references a non-existent provider.
              The save committed (no error), but the operator needs to act
              on the warning before the config will work at runtime. */}
          {warnings.length > 0 && (
            <div
              className="mb-2 rounded text-sm border px-3 py-2"
              style={{
                borderColor: 'var(--color-status-warning, #facc15)',
                color: 'var(--color-status-warning, #facc15)',
                background: 'color-mix(in srgb, var(--color-status-warning, #facc15) 10%, transparent)',
              }}
            >
              <div className="font-medium mb-1">
                ⚠ {formatMessage(t('config.fields.warning_count'), { count: warnings.length })}
              </div>
              <ul className="list-disc pl-5 space-y-1">
                {warnings.map((w) => (
                  <li key={`${w.code}:${w.path}`}>
                    <span className="font-mono text-xs opacity-80">[{w.code}]</span>{' '}
                    {w.message}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <div className="flex items-center justify-between gap-3">
            <div className="flex-1 min-w-0 text-sm">
              {topError ? (
                <span style={{ color: 'var(--color-status-error)' }}>
                  ⚠ {topError}
                </span>
              ) : savedAt ? (
                <span style={{ color: 'var(--color-status-success)' }}>
                  ✓ {savedAt}
                </span>
              ) : unsavedCount > 0 ? (
                <span style={{ color: 'var(--pc-text-secondary)' }}>
                  {formatMessage(t('config.fields.unsaved_count'), { count: unsavedCount })}
                </span>
              ) : (
                <span style={{ color: 'var(--pc-text-faint)' }}>
                  {t('config.fields.no_unsaved_changes')}
                </span>
              )}
            </div>
            <button
              type="button"
              onClick={() => void handleSave()}
              disabled={saving || unsavedCount === 0}
              className="btn-electric flex items-center gap-2 text-sm px-4 py-2 flex-shrink-0"
            >
              <Save className="h-4 w-4" />
              {saving ? t('common.saving') : t('common.save')}
            </button>
          </div>
        </div>
      )}
    </div>
  );
});

export default FieldForm;

interface FieldRowProps {
  entry: ListResponseEntry;
  value: string;
  onChange: (v: string) => void;
  comment: string;
  onCommentChange: (v: string) => void;
  error: ConfigApiError | undefined;
  onDelete?: () => void;
  /** `///` doc comment resolved from the cached JSON Schema for this path. */
  description: string | null;
  /** Per-element property metadata for `kind === 'object-array'` fields. */
  elementProps?: ObjectArrayPropMeta[] | null;
  /** Drift entry for this path (in-memory ≠ on-disk). `null` when no drift. */
  drift: DriftEntry | null;
}

function FieldRow({ entry, value, onChange, comment, onCommentChange, error, onDelete, description, elementProps, drift }: FieldRowProps) {
  const renderer = rendererFor(entry);
  const [providerModels, setProviderModels] = useState<string[] | null>(null);
  const [modelsFetchFailed, setModelsFetchFailed] = useState(false);
  const isProviderModelField = /^providers\.models\.[^.]+\.model$/.test(entry.path);
  const label = tConfigLabel(entry.path, fieldShortLabel(entry));

  useEffect(() => {
    if (!isProviderModelField) return;
    const provider = entry.path.split('.')[2];
    if (!provider) return;
    const cached = modelsCache[provider];
    if (cached) {
      setProviderModels(cached.models);
      setModelsFetchFailed(!cached.live && cached.models.length === 0);
      return;
    }
    void getCatalogModels(provider)
      .then((r) => {
        modelsCache[provider] = { models: r.models, live: r.live };
        setProviderModels(r.models);
        setModelsFetchFailed(!r.live && r.models.length === 0);
      })
      .catch(() => {
        modelsCache[provider] = { models: [], live: false };
        setProviderModels([]);
        setModelsFetchFailed(true);
      });
  }, [isProviderModelField, entry.path]);

  return (
    <div className="px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <div className="flex-1 min-w-0">
          <label
            className="block text-sm font-medium break-all"
            style={{ color: 'var(--pc-text-primary)' }}
            htmlFor={entry.path}
            title={entry.type_hint}
          >
            {label}
            {entry.is_secret && (
              <span
                className="ml-2 text-xs font-sans"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                🔒 {entry.populated ? t('config.fields.secret_set') : t('config.fields.secret_unset')}
              </span>
            )}
          </label>
          <code
            className="block text-[11px] mt-0.5 break-all"
            style={{ color: 'var(--pc-text-faint)' }}
          >
            {entry.path}
          </code>
          {description && (
            <p
              className="text-xs mt-0.5"
              style={{ color: 'var(--pc-text-secondary)' }}
            >
              {description}
            </p>
          )}
          {drift && <DriftDiff drift={drift} />}
        </div>
        {onDelete && (
          <button
            type="button"
            onClick={onDelete}
            title={t('config.fields.reset_title')}
            className="btn-icon flex-shrink-0"
          >
            <Trash2 className="h-4 w-4" />
          </button>
        )}
      </div>

      <div className="mt-2 space-y-1.5">
        {renderer === 'bool' ? (
          <select
            id={entry.path}
            value={value || 'false'}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm appearance-none cursor-pointer"
          >
            <option value="true">{t('common.yes')}</option>
            <option value="false">{t('common.no')}</option>
          </select>
        ) : renderer === 'select' ? (
          <select
            id={entry.path}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm appearance-none cursor-pointer"
          >
            <option value="">—</option>
            {(entry.enum_variants ?? []).map((v) => (
              <option key={v} value={v}>
                {v}
              </option>
            ))}
          </select>
        ) : isProviderModelField && providerModels !== null && providerModels.length > 0 ? (
          <>
            <input
              id={entry.path}
              list={`models-${entry.path}`}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
              placeholder={tConfigPlaceholder(entry.path, t('config.placeholder.provider_model'))}
            />
            <datalist id={`models-${entry.path}`}>
              {providerModels.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
          </>
        ) : isProviderModelField && modelsFetchFailed ? (
          // Fetch failed — fall back to free text with explicit help.
          <>
            <input
              id={entry.path}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
              placeholder={t('config.placeholder.provider_model_unreachable')}
            />
            <p
              className="text-xs"
              style={{ color: 'var(--pc-text-muted)' }}
            >
              {t('config.fields.model_catalog_unreachable')}{' '}
              <code>claude-sonnet-4-5-20251101</code>.
            </p>
          </>
        ) : isProviderModelField && providerModels === null ? (
          // Fetching catalog…
          <>
            <input
              id={entry.path}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
              placeholder={t('config.placeholder.fetching_models')}
              disabled
            />
            <p
              className="text-xs"
              style={{ color: 'var(--pc-text-muted)' }}
            >
              {t('config.fields.fetching_models')}
            </p>
          </>
        ) : renderer === 'array' ? (
          <ArrayFieldEditor
            inputId={entry.path}
            value={value}
            onChange={onChange}
            isOptional={isOptionalArray(entry.type_hint)}
          />
        ) : renderer === 'object-array' ? (
          <ObjectArrayEditor
            inputId={entry.path}
            value={value}
            onChange={onChange}
            elementProps={elementProps ?? null}
          />
        ) : renderer === 'number' ? (
          <input
            id={entry.path}
            type="number"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm"
          />
        ) : (
          <input
            id={entry.path}
            type={renderer === 'secret' ? 'password' : 'text'}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm"
            placeholder={
              renderer === 'secret'
                ? entry.populated
                  ? t('config.placeholder.secret_keep')
                  : t('config.placeholder.secret_enter')
                : ''
            }
          />
        )}

        <input
          type="text"
          value={comment}
          onChange={(e) => onCommentChange(e.target.value)}
          placeholder={t('config.placeholder.comment')}
          className="input-electric w-full px-3 py-1.5 text-xs"
          style={{ color: 'var(--pc-text-secondary)' }}
        />

        {error && (
          <p className="mt-1 text-sm" style={{ color: 'var(--color-status-error)' }}>
            <span className="font-mono text-xs">{error.code}</span>: {error.message}
          </p>
        )}
      </div>
    </div>
  );
}

interface ArrayFieldEditorProps {
  inputId: string;
  value: string;
  onChange: (next: string) => void;
  isOptional: boolean;
}

// Per-row chip editor for `Vec<String>` / `Option<Vec<String>>` fields with
// a "Rows / Text" toggle. Both views share the same underlying value (a
// JSON array string) so toggling preserves edits. Trim + drop-empty runs
// at save time in `parseArrayDraft`, not on every keystroke — typing a
// space inside a chip shouldn't truncate the entry.
function ArrayFieldEditor({ inputId, value, onChange, isOptional }: ArrayFieldEditorProps) {
  const [mode, setMode] = useState<'rows' | 'text'>('rows');
  const rows = useMemo(() => parseArrayRows(value), [value]);

  const writeRows = (next: string[]) => {
    onChange(JSON.stringify(next));
  };

  const setRow = (index: number, next: string) => {
    writeRows(rows.map((r, i) => (i === index ? next : r)));
  };

  const removeRow = (index: number) => {
    writeRows(rows.filter((_, i) => i !== index));
  };

  const addRow = () => {
    writeRows([...rows, '']);
  };

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>
          {formatMessage(t('config.fields.entry_count'), { count: rows.length })}
          {isOptional && rows.length === 0 ? ` — ${t('config.fields.saves_as_null')}` : null}
        </span>
        <div
          className="inline-flex rounded-md overflow-hidden border text-xs"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          <button
            type="button"
            onClick={() => setMode('rows')}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background: mode === 'rows' ? 'var(--pc-bg-surface-elevated)' : 'transparent',
              color: mode === 'rows' ? 'var(--pc-text-primary)' : 'var(--pc-text-muted)',
            }}
            aria-pressed={mode === 'rows'}
          >
            <ListIcon className="h-3 w-3" /> {t('config.fields.rows_mode')}
          </button>
          <button
            type="button"
            onClick={() => setMode('text')}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background: mode === 'text' ? 'var(--pc-bg-surface-elevated)' : 'transparent',
              color: mode === 'text' ? 'var(--pc-text-primary)' : 'var(--pc-text-muted)',
            }}
            aria-pressed={mode === 'text'}
          >
            <TypeIcon className="h-3 w-3" /> {t('config.fields.text_mode')}
          </button>
        </div>
      </div>

      {mode === 'rows' ? (
        <>
          {rows.length === 0 ? (
            <p
              className="text-xs italic px-1 py-2"
              style={{ color: 'var(--pc-text-faint)' }}
            >
              {t('config.fields.no_entries_add')}
            </p>
          ) : (
            <ul className="space-y-1.5" id={inputId}>
              {rows.map((row, i) => (
                <li key={i} className="flex items-center gap-2">
                  <input
                    type="text"
                    value={row}
                    onChange={(e) => setRow(i, e.target.value)}
                    className="input-electric flex-1 px-3 py-1.5 text-sm"
                    placeholder={t('config.placeholder.empty')}
                  />
                  <button
                    type="button"
                    onClick={() => removeRow(i)}
                    title={t('config.fields.remove_entry')}
                    className="btn-icon flex-shrink-0"
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            onClick={addRow}
            className="btn-secondary text-xs px-3 py-1.5 inline-flex items-center gap-1"
          >
            <Plus className="h-3 w-3" /> {t('common.add')}
          </button>
        </>
      ) : (
        <textarea
          id={inputId}
          rows={Math.max(3, Math.min(rows.length + 1, 10))}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
          placeholder={t('config.placeholder.array_json')}
        />
      )}
    </div>
  );
}

interface ObjectArrayEditorProps {
  inputId: string;
  /** JSON-array string of objects. Empty/`<unset>`/invalid JSON normalize to `[]`. */
  value: string;
  onChange: (next: string) => void;
  /** Per-property metadata for the element type, walked from the JSON Schema.
   *  `null` when the schema isn't loaded yet or the element shape can't be
   *  resolved — falls back to a raw JSON textarea. */
  elementProps: ObjectArrayPropMeta[] | null;
}

// Per-row form editor for `Vec<T>` of structs (e.g. `mcp.servers`).
// Parses the JSON-array value, renders one row per element with per-property
// inputs derived from the JSON Schema, and serializes back to JSON on save.
// Schema v3 / #5947 will migrate the load-bearing Vecs to `HashMap<String, T>`
// keyed tables; this editor is the bridge so the dashboard doesn't have to
// wait on that to surface MCP servers / peripheral boards / etc.
function ObjectArrayEditor({ inputId, value, onChange, elementProps }: ObjectArrayEditorProps) {
  const rows = useMemo<Record<string, unknown>[]>(() => {
    try {
      const parsed = JSON.parse(value || '[]');
      if (Array.isArray(parsed)) {
        return parsed.filter((r): r is Record<string, unknown> => typeof r === 'object' && r !== null);
      }
    } catch {
      /* fall through */
    }
    return [];
  }, [value]);

  const writeRows = (next: Record<string, unknown>[]) => {
    onChange(JSON.stringify(next));
  };

  const setField = (rowIdx: number, key: string, raw: unknown) => {
    const next = rows.map((r, i) => (i === rowIdx ? { ...r, [key]: raw } : r));
    writeRows(next);
  };

  const removeRow = (rowIdx: number) => {
    writeRows(rows.filter((_, i) => i !== rowIdx));
  };

  const addRow = () => {
    // Seed required-string keys with empty strings so the row renders an
    // empty input rather than nothing.
    const seed: Record<string, unknown> = {};
    if (elementProps) {
      for (const p of elementProps) {
        if (p.kind === 'string' && !p.optional) seed[p.key] = '';
      }
    }
    writeRows([...rows, seed]);
  };

  // Schema not loaded or unresolvable: degrade to a raw JSON textarea so
  // the field is still editable. Visually distinct so users see why.
  if (!elementProps || elementProps.length === 0) {
    return (
      <div className="space-y-1.5">
        <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
          {t('config.fields.raw_json_fallback')}
        </p>
        <textarea
          id={inputId}
          rows={Math.max(4, Math.min(rows.length * 4 + 2, 16))}
          value={value || '[]'}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
          placeholder={t('config.placeholder.object_array_json')}
        />
      </div>
    );
  }

  return (
    <div className="space-y-2" id={inputId}>
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>
          {formatMessage(t('config.fields.entry_count'), { count: rows.length })}
        </span>
        <button
          type="button"
          onClick={addRow}
          className="btn-secondary text-xs px-3 py-1.5 inline-flex items-center gap-1"
        >
          <Plus className="h-3 w-3" /> {t('common.add')}
        </button>
      </div>
      {rows.length === 0 ? (
        <p className="text-xs italic px-1 py-2" style={{ color: 'var(--pc-text-faint)' }}>
          {t('config.fields.no_entries_create')}
        </p>
      ) : (
        <ul className="space-y-3">
          {rows.map((row, rowIdx) => (
            <li
              key={rowIdx}
              className="rounded-md border p-3 space-y-2"
              style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-base)' }}
            >
              <div className="flex items-center justify-between">
                <span className="text-xs font-mono" style={{ color: 'var(--pc-text-faint)' }}>
                  [{rowIdx}]
                  {typeof row.name === 'string' && row.name.length > 0 && (
                    <span className="ml-2" style={{ color: 'var(--pc-text-secondary)' }}>
                      {row.name}
                    </span>
                  )}
                </span>
                <button
                  type="button"
                  onClick={() => removeRow(rowIdx)}
                  title={t('config.fields.remove_entry')}
                  className="btn-icon"
                >
                  <Trash2 className="h-4 w-4" />
                </button>
              </div>
              {elementProps.map((p) => (
                <ObjectArrayField
                  key={p.key}
                  parentPath={inputId}
                  meta={p}
                  rawValue={row[p.key]}
                  onChange={(v) => setField(rowIdx, p.key, v)}
                />
              ))}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ObjectArrayField({
  parentPath,
  meta,
  rawValue,
  onChange,
}: {
  parentPath: string;
  meta: ObjectArrayPropMeta;
  rawValue: unknown;
  onChange: (next: unknown) => void;
}) {
  const description = tConfigDescription(`${parentPath}.${meta.key}`, meta.description);
  const label = tConfigLabel(`${parentPath}.${meta.key}`, meta.label);
  const display = (() => {
    if (rawValue === null || rawValue === undefined) return '';
    if (typeof rawValue === 'string') return rawValue;
    if (typeof rawValue === 'number' || typeof rawValue === 'boolean') return String(rawValue);
    return JSON.stringify(rawValue);
  })();
  return (
    <div>
      <label className="block text-xs font-mono" style={{ color: 'var(--pc-text-secondary)' }}>
        {label}
        {meta.optional && (
          <span className="ml-1.5 text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>
            {t('config.fields.optional')}
          </span>
        )}
      </label>
      {description && (
        <p className="text-[11px] mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>
          {description}
        </p>
      )}
      {meta.kind === 'bool' ? (
        <select
          value={display || 'false'}
          onChange={(e) => onChange(e.target.value === 'true')}
          className="input-electric w-full px-2 py-1 mt-1 text-sm appearance-none cursor-pointer"
        >
          <option value="true">{t('common.yes')}</option>
          <option value="false">{t('common.no')}</option>
        </select>
      ) : meta.kind === 'enum' && meta.enumVariants ? (
        <select
          value={display}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-2 py-1 mt-1 text-sm appearance-none cursor-pointer"
        >
          <option value="">—</option>
          {meta.enumVariants.map((v) => (
            <option key={v} value={v}>{v}</option>
          ))}
        </select>
      ) : meta.kind === 'integer' || meta.kind === 'float' ? (
        <input
          type="number"
          value={display}
          onChange={(e) => {
            const n = Number(e.target.value);
            onChange(Number.isNaN(n) || e.target.value === '' ? null : n);
          }}
          className="input-electric w-full px-2 py-1 mt-1 text-sm"
        />
      ) : meta.kind === 'string-array' ? (
        // Same chip + text-mode editor the top-level FieldForm uses for
        // Vec<String> fields. Bridges its JSON-string contract to/from
        // the row object's array-typed value: rows-mode edits emit valid
        // JSON arrays we can parse into the row property; mid-edit text
        // mode stores the in-progress string verbatim, deferring shape
        // validation to save time (same way the top-level path does).
        <ArrayFieldEditor
          inputId={`${meta.key}`}
          value={
            Array.isArray(rawValue)
              ? JSON.stringify(rawValue)
              : typeof rawValue === 'string'
                ? rawValue
                : '[]'
          }
          onChange={(s) => {
            try {
              const parsed = JSON.parse(s);
              if (Array.isArray(parsed)) {
                onChange(parsed);
                return;
              }
            } catch {
              /* fall through */
            }
            onChange(s);
          }}
          isOptional={meta.optional}
        />
      ) : meta.kind === 'object' ? (
        <KeyValueChipEditor
          pairs={
            typeof rawValue === 'object' && rawValue !== null && !Array.isArray(rawValue)
              ? Object.entries(rawValue as Record<string, unknown>).map(
                  ([k, v]) => [k, typeof v === 'string' ? v : JSON.stringify(v)] as [string, string],
                )
              : []
          }
          onChange={(pairs) => onChange(Object.fromEntries(pairs))}
        />
      ) : (
        <input
          type="text"
          value={display}
          onChange={(e) => onChange(e.target.value)}
          className="input-electric w-full px-2 py-1 mt-1 text-sm"
        />
      )}
    </div>
  );
}

// Compact key-value chip editor for `HashMap<String, String>`
// properties inside an object-array row (e.g. `mcp.servers[i].env`,
// `headers`). Mirrors `ArrayFieldEditor`'s Rows / Text toggle so a
// power user can hand-edit the JSON object form when chips get
// unwieldy. Mid-edit invalid JSON is preserved in the textarea (no
// input fight); pairs only update when the buffer parses to an object.
function KeyValueChipEditor({
  pairs,
  onChange,
}: {
  pairs: [string, string][];
  onChange: (next: [string, string][]) => void;
}) {
  const [mode, setMode] = useState<'rows' | 'text'>('rows');
  // Local textarea buffer — only consulted in `text` mode. Reset when
  // the user re-enters text mode so the buffer reflects current pairs;
  // cleared when leaving text mode so re-entry shows fresh JSON.
  const [textDraft, setTextDraft] = useState<string | null>(null);

  const setKey = (i: number, k: string) => {
    onChange(pairs.map((p, idx) => (idx === i ? [k, p[1]] : p)));
  };
  const setValue = (i: number, v: string) => {
    onChange(pairs.map((p, idx) => (idx === i ? [p[0], v] : p)));
  };
  const removeAt = (i: number) => {
    onChange(pairs.filter((_, idx) => idx !== i));
  };

  const switchToRows = () => {
    setTextDraft(null);
    setMode('rows');
  };
  const switchToText = () => {
    setTextDraft(JSON.stringify(Object.fromEntries(pairs), null, 2));
    setMode('text');
  };

  return (
    <div className="space-y-1.5 mt-1">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>
          {formatMessage(t('config.fields.entry_count'), { count: pairs.length })}
        </span>
        <div
          className="inline-flex rounded-md overflow-hidden border text-xs"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          <button
            type="button"
            onClick={switchToRows}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background: mode === 'rows' ? 'var(--pc-bg-surface-elevated)' : 'transparent',
              color: mode === 'rows' ? 'var(--pc-text-primary)' : 'var(--pc-text-muted)',
            }}
            aria-pressed={mode === 'rows'}
          >
            <ListIcon className="h-3 w-3" /> {t('config.fields.rows_mode')}
          </button>
          <button
            type="button"
            onClick={switchToText}
            className="px-2 py-1 inline-flex items-center gap-1"
            style={{
              background: mode === 'text' ? 'var(--pc-bg-surface-elevated)' : 'transparent',
              color: mode === 'text' ? 'var(--pc-text-primary)' : 'var(--pc-text-muted)',
            }}
            aria-pressed={mode === 'text'}
          >
            <TypeIcon className="h-3 w-3" /> {t('config.fields.text_mode')}
          </button>
        </div>
      </div>

      {mode === 'text' ? (
        <textarea
          rows={Math.max(3, Math.min(pairs.length + 2, 10))}
          value={textDraft ?? JSON.stringify(Object.fromEntries(pairs), null, 2)}
          onChange={(e) => {
            const v = e.target.value;
            setTextDraft(v);
            try {
              const parsed = JSON.parse(v);
              if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
                onChange(
                  Object.entries(parsed as Record<string, unknown>).map(
                    ([k, val]) =>
                      [k, typeof val === 'string' ? val : JSON.stringify(val)] as [string, string],
                  ),
                );
              }
            } catch {
              /* keep textDraft until valid JSON */
            }
          }}
          className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
          placeholder={t('config.placeholder.object_json')}
        />
      ) : (
        <>
          {pairs.length === 0 ? (
            <p className="text-[11px] italic" style={{ color: 'var(--pc-text-faint)' }}>
              {t('config.fields.no_entries')}
            </p>
          ) : (
            <ul className="space-y-1">
              {pairs.map(([k, v], i) => (
                <li key={i} className="flex items-center gap-2">
                  <input
                    type="text"
                    value={k}
                    onChange={(e) => setKey(i, e.target.value)}
                    className="input-electric flex-1 px-2 py-1 text-sm font-mono"
                    placeholder={t('config.placeholder.key')}
                  />
                  <span style={{ color: 'var(--pc-text-faint)' }}>=</span>
                  <input
                    type="text"
                    value={v}
                    onChange={(e) => setValue(i, e.target.value)}
                    className="input-electric flex-1 px-2 py-1 text-sm"
                    placeholder={t('config.placeholder.value')}
                  />
                  <button
                    type="button"
                    onClick={() => removeAt(i)}
                    title={t('config.fields.remove_entry')}
                    className="btn-icon flex-shrink-0"
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            onClick={() => onChange([...pairs, ['', '']])}
            className="btn-secondary text-xs px-2.5 py-1 inline-flex items-center gap-1"
          >
            <Plus className="h-3 w-3" /> {t('common.add')}
          </button>
        </>
      )}
    </div>
  );
}

// Per-field drift indicator: small inline pill showing in-memory vs
// on-disk values side by side. Secret-marked paths surface only the
// fact of drift — values never leave the server (server-side hash
// compare in `compute_drift`).
function DriftDiff({ drift }: { drift: DriftEntry }) {
  if (drift.secret) {
    return (
      <p
        className="text-xs mt-1 inline-flex items-center gap-1"
        style={{ color: 'var(--color-status-warning, #f5b400)' }}
      >
        ⚠ {t('config.drift.secret_differs')}
      </p>
    );
  }
  const inMem = formatDriftValue(drift.in_memory_value);
  const onDisk = formatDriftValue(drift.on_disk_value);
  return (
    <div
      className="text-xs mt-1 flex flex-wrap gap-x-3 gap-y-0.5"
      style={{ color: 'var(--color-status-warning, #f5b400)' }}
    >
      <span>
        {t('config.drift.in_memory')}:{' '}
        <code style={{ color: 'var(--pc-text-secondary)' }}>{inMem}</code>
      </span>
      <span>
        {t('config.drift.on_disk')}:{' '}
        <code style={{ color: 'var(--pc-text-secondary)' }}>{onDisk}</code>
      </span>
    </div>
  );
}

function formatDriftValue(value: unknown): string {
  if (value === null || value === undefined) return '<unset>';
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}
