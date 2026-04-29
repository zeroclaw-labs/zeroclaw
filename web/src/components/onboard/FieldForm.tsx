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

import { useEffect, useMemo, useState } from 'react';
import { Save, Trash2 } from 'lucide-react';
import {
  ApiError,
  deleteProp,
  getCatalogModels,
  listProps,
  patchConfig,
  type ConfigApiError,
  type ListResponseEntry,
  type PatchOp,
} from '../../lib/api';

interface FieldFormProps {
  /** Dotted prefix to fetch fields under, e.g. `providers.models.anthropic`. */
  prefix: string;
  /** Called after a successful save; parent typically advances or refreshes. */
  onSaved?: () => void;
  /** Hide the trash icon (per-prop reset) when the parent doesn't want it. */
  showDelete?: boolean;
  /** Optional title rendered above the form. */
  title?: string;
}

function rendererFor(entry: ListResponseEntry): 'bool' | 'array' | 'secret' | 'select' | 'number' | 'text' {
  if (entry.is_secret) return 'secret';
  switch (entry.kind) {
    case 'bool':
      return 'bool';
    case 'string-array':
      return 'array';
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

function defaultInputValue(entry: ListResponseEntry): string {
  const v = entry.value;
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
      return raw.split('\n').map((s) => s.trim()).filter(Boolean);
    case 'number': {
      const n = Number(raw);
      return Number.isNaN(n) ? raw : n;
    }
    default:
      return raw;
  }
}

const modelsCache: Record<string, { models: string[]; live: boolean }> = {};

export default function FieldForm({ prefix, onSaved, showDelete = true, title }: FieldFormProps) {
  const [entries, setEntries] = useState<ListResponseEntry[]>([]);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [comments, setComments] = useState<Record<string, string>>({});
  const [fieldErrors, setFieldErrors] = useState<Record<string, ConfigApiError>>({});
  const [topError, setTopError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);

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

  const handleSave = async () => {
    setSaving(true);
    setSavedAt(null);
    setTopError(null);
    setFieldErrors({});

    const ops: PatchOp[] = [];
    for (const e of entries) {
      const raw = draft[e.path] ?? '';
      const original = defaultInputValue(e);
      // Secrets with empty input mean "don't change".
      if (e.is_secret && raw.length === 0) continue;
      if (raw === original) continue;
      const op: PatchOp = { op: 'replace', path: e.path, value: parseInput(e, raw) };
      const c = comments[e.path];
      if (c && c.length > 0) op.comment = c;
      ops.push(op);
    }

    if (ops.length === 0) {
      setSavedAt('No changes to save.');
      setSaving(false);
      return;
    }

    try {
      const resp = await patchConfig(ops);
      setSavedAt(`Saved ${resp.results.length} field(s).`);
      await reload();
      onSaved?.();
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
    } finally {
      setSaving(false);
    }
  };

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
    // Stable order: secrets first (most-needed), then alphabetical by short label.
    return [...entries].sort((a, b) => {
      if (a.is_secret !== b.is_secret) return a.is_secret ? -1 : 1;
      return fieldShortLabel(a).localeCompare(fieldShortLabel(b));
    });
  }, [entries]);

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

      {entries.length === 0 ? (
        <div
          className="surface-panel p-6 text-center text-sm"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          No fields under <code style={{ color: 'var(--pc-text-faint)' }}>{prefix}</code>.
        </div>
      ) : (
        <form
          className="surface-panel divide-y"
          style={{ borderColor: 'var(--pc-border)' }}
          onSubmit={(e) => {
            e.preventDefault();
            void handleSave();
          }}
        >
          {sortedEntries.map((f) => (
            <FieldRow
              key={f.path}
              entry={f}
              value={draft[f.path] ?? ''}
              onChange={(v) => setDraft((d) => ({ ...d, [f.path]: v }))}
              comment={comments[f.path] ?? ''}
              onCommentChange={(v) => setComments((c) => ({ ...c, [f.path]: v }))}
              error={fieldErrors[f.path]}
              onDelete={showDelete ? () => handleDelete(f.path) : undefined}
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
                  {unsavedCount} unsaved {unsavedCount === 1 ? 'change' : 'changes'}
                </span>
              ) : (
                <span style={{ color: 'var(--pc-text-faint)' }}>
                  No unsaved changes
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
              {saving ? 'Saving…' : 'Save'}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

interface FieldRowProps {
  entry: ListResponseEntry;
  value: string;
  onChange: (v: string) => void;
  comment: string;
  onCommentChange: (v: string) => void;
  error: ConfigApiError | undefined;
  onDelete?: () => void;
}

function FieldRow({ entry, value, onChange, comment, onCommentChange, error, onDelete }: FieldRowProps) {
  const renderer = rendererFor(entry);
  const [providerModels, setProviderModels] = useState<string[] | null>(null);
  const [modelsFetchFailed, setModelsFetchFailed] = useState(false);
  const isProviderModelField = /^providers\.models\.[^.]+\.model$/.test(entry.path);

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
            className="block text-sm font-medium"
            style={{ color: 'var(--pc-text-primary)' }}
            htmlFor={entry.path}
          >
            {fieldShortLabel(entry)}
            {entry.is_secret && (
              <span
                className="ml-2 text-xs"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                🔒 {entry.populated ? 'set' : 'unset'}
              </span>
            )}
          </label>
          <code
            className="block text-xs mt-0.5 break-all"
            style={{ color: 'var(--pc-text-faint)' }}
          >
            {entry.path}{' '}
            <span style={{ opacity: 0.6 }}>({entry.type_hint})</span>
          </code>
        </div>
        {onDelete && (
          <button
            type="button"
            onClick={onDelete}
            title="Reset to default / unset"
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
            <option value="true">true</option>
            <option value="false">false</option>
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
              placeholder="Pick from list or type a model name"
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
              placeholder="Type a model identifier (catalog unreachable)"
            />
            <p
              className="text-xs"
              style={{ color: 'var(--pc-text-muted)' }}
            >
              Could not fetch model catalog for this provider. Type the
              identifier from your provider's docs (e.g.{' '}
              <code>claude-sonnet-4-5-20251101</code>).
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
              placeholder="Fetching models…"
              disabled
            />
            <p
              className="text-xs"
              style={{ color: 'var(--pc-text-muted)' }}
            >
              Fetching available models from the provider's catalog…
            </p>
          </>
        ) : renderer === 'array' ? (
          <textarea
            id={entry.path}
            rows={3}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="input-electric w-full px-3 py-2 text-sm font-mono resize-y"
            placeholder="One value per line"
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
                  ? 'Leave blank to keep current value'
                  : 'Enter value'
                : entry.type_hint
            }
          />
        )}

        <input
          type="text"
          value={comment}
          onChange={(e) => onCommentChange(e.target.value)}
          placeholder="Optional comment (why?)"
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
