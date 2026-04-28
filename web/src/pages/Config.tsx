// Schema-driven configuration editor (issue #6175).
//
// Renders every field discoverable from `GET /api/config/list` grouped by
// category. NO hardcoded section names, field labels, or dropdown options —
// everything comes from the gateway's PropFieldInfo (kind, type_hint,
// enum_variants, description). Adding a new field anywhere in the schema
// makes it appear here automatically.
//
// "+ Add" affordances come from `GET /api/config/templates`, which the
// gateway derives from `Configurable::map_key_sections()` — single source of
// truth, no hand-maintained list. Provider picker uses the upstream catalog
// the CLI wizard fetches (`GET /api/onboard/catalog`).

import { useEffect, useMemo, useState } from 'react';
import { Plus, Save, Settings, Trash2, X } from 'lucide-react';
import {
  ApiError,
  createMapKey,
  deleteProp,
  getCatalog,
  getCatalogModels,
  getTemplates,
  listProps,
  patchConfig,
  type CatalogProvider,
  type ConfigApiError,
  type ListResponseEntry,
  type PatchOp,
  type TemplateEntry,
} from '../lib/api';

type Group = {
  category: string;
  entries: ListResponseEntry[];
};

function groupByCategory(entries: ListResponseEntry[]): Group[] {
  const map = new Map<string, ListResponseEntry[]>();
  for (const e of entries) {
    const arr = map.get(e.category) ?? [];
    arr.push(e);
    map.set(e.category, arr);
  }
  return Array.from(map.entries())
    .map(([category, entries]) => ({ category, entries }))
    .sort((a, b) => a.category.localeCompare(b.category));
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

function fieldLabel(entry: ListResponseEntry): string {
  const tail = entry.path.split('.').slice(-2).join(' ');
  return tail.replace(/[-_]/g, ' ');
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

const modelsCache: Record<string, string[]> = {};

export default function Config() {
  const [entries, setEntries] = useState<ListResponseEntry[]>([]);
  const [templates, setTemplates] = useState<TemplateEntry[]>([]);
  const [catalog, setCatalog] = useState<CatalogProvider[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [comments, setComments] = useState<Record<string, string>>({});
  const [fieldErrors, setFieldErrors] = useState<Record<string, ConfigApiError>>({});
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState<string | null>(null);
  const [addModalTemplate, setAddModalTemplate] = useState<TemplateEntry | null>(null);
  const [addKey, setAddKey] = useState('');
  const [addError, setAddError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);

  const reload = async () => {
    setLoading(true);
    setError(null);
    try {
      const [list, tpl, cat] = await Promise.all([
        listProps(),
        getTemplates(),
        getCatalog().catch(() => ({ providers: [] })),
      ]);
      setEntries(list.entries);
      setTemplates(tpl.templates);
      setCatalog(cat.providers);
      const seed: Record<string, string> = {};
      for (const e of list.entries) seed[e.path] = defaultInputValue(e);
      setDraft(seed);
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
  }, []);

  const groups = useMemo(() => groupByCategory(entries), [entries]);

  const handleSaveAll = async () => {
    setSaving(true);
    setSavedAt(null);
    setFieldErrors({});

    const ops: PatchOp[] = [];
    for (const e of entries) {
      const raw = draft[e.path] ?? '';
      const original = defaultInputValue(e);
      if (e.is_secret && raw.length === 0) continue;
      if (raw === original) continue;
      const op: PatchOp = { op: 'replace', path: e.path, value: parseInput(e, raw) };
      const c = comments[e.path];
      if (c && c.length > 0) op.comment = c;
      ops.push(op);
    }

    if (ops.length === 0) {
      setSaving(false);
      setSavedAt('No changes to save.');
      return;
    }

    try {
      const resp = await patchConfig(ops);
      setSavedAt(`Saved ${resp.results.length} field(s) at ${new Date().toLocaleTimeString()}.`);
      await reload();
    } catch (e) {
      if (e instanceof ApiError) {
        const env = e.envelope as ConfigApiError;
        if (env.path) {
          setFieldErrors({ [env.path]: env });
        } else {
          setError(`[${env.code}] ${env.message}`);
        }
      } else {
        setError(String(e instanceof Error ? e.message : e));
      }
    } finally {
      setSaving(false);
    }
  };

  const handleAdd = async () => {
    if (!addModalTemplate) return;
    const key = addKey.trim();
    if (!key) return;
    setAdding(true);
    setAddError(null);
    try {
      await createMapKey(addModalTemplate.path, key);
      setAddModalTemplate(null);
      setAddKey('');
      await reload();
    } catch (e) {
      if (e instanceof ApiError) {
        const env = e.envelope as ConfigApiError;
        setAddError(`[${env.code}] ${env.message}`);
      } else {
        setAddError(String(e instanceof Error ? e.message : e));
      }
    } finally {
      setAdding(false);
    }
  };

  const handleDelete = async (path: string) => {
    try {
      await deleteProp(path);
      await reload();
    } catch (e) {
      if (e instanceof ApiError) {
        const env = e.envelope as ConfigApiError;
        setError(`[${env.code}] ${env.message}`);
      } else {
        setError(String(e instanceof Error ? e.message : e));
      }
    }
  };

  const ensureModelsLoaded = async (provider: string) => {
    if (modelsCache[provider]) return modelsCache[provider];
    try {
      const r = await getCatalogModels(provider);
      modelsCache[provider] = r.models;
      return r.models;
    } catch {
      modelsCache[provider] = [];
      return [];
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full p-6 gap-4 animate-fade-in overflow-y-auto">
      {/* Header — matches the convention used by Memory.tsx, Tools.tsx, etc. */}
      <div className="flex items-center justify-between flex-shrink-0">
        <div className="flex items-center gap-2">
          <Settings className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Configuration
          </h2>
        </div>
        <button
          onClick={handleSaveAll}
          disabled={saving}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          <Save className="h-4 w-4" />
          {saving ? 'Saving…' : 'Save all changes'}
        </button>
      </div>

      {/* Status banners */}
      {error && (
        <div
          className="rounded-xl border p-3 text-sm animate-fade-in flex-shrink-0"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {error}
        </div>
      )}
      {savedAt && (
        <div
          className="rounded-xl border p-3 text-sm animate-fade-in flex-shrink-0"
          style={{
            background: 'rgba(0, 230, 138, 0.06)',
            borderColor: 'rgba(0, 230, 138, 0.2)',
            color: 'var(--color-status-success)',
          }}
        >
          {savedAt}
        </div>
      )}

      {/* "+ Add new entry" — one button per addable section. Clicking opens
          a modal so the input flow is unambiguous (no inline-input puzzle). */}
      {templates.length > 0 && (
        <div className="surface-panel p-4 flex-shrink-0">
          <h3
            className="text-xs font-semibold uppercase tracking-wider mb-3"
            style={{ color: 'var(--pc-text-secondary)' }}
          >
            Add new entry
          </h3>
          <div className="flex flex-wrap gap-2">
            {templates.map((t) => (
              <button
                key={t.path}
                onClick={() => {
                  setAddModalTemplate(t);
                  setAddKey('');
                  setAddError(null);
                }}
                className="btn-secondary flex items-center gap-2 text-xs px-3 py-2"
                title={t.description || `${t.kind}: ${t.value_type}`}
              >
                <Plus className="h-3 w-3" />
                <span style={{ color: 'var(--pc-text-primary)' }}>{t.path}</span>
                <span style={{ color: 'var(--pc-text-muted)' }}>· {t.value_type}</span>
              </button>
            ))}
          </div>
        </div>
      )}

      {/* Add modal — same dialog pattern as Memory's add-entry modal. */}
      {addModalTemplate && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="surface-panel p-6 w-full max-w-md mx-4 animate-fade-in-scale">
            <div className="flex items-center justify-between mb-4">
              <h3
                className="text-lg font-semibold"
                style={{ color: 'var(--pc-text-primary)' }}
              >
                Add to <code style={{ color: 'var(--pc-accent)' }}>{addModalTemplate.path}</code>
              </h3>
              <button
                onClick={() => {
                  setAddModalTemplate(null);
                  setAddKey('');
                  setAddError(null);
                }}
                className="btn-icon"
              >
                <X className="h-5 w-5" />
              </button>
            </div>
            {addModalTemplate.description && (
              <p
                className="text-sm mb-4"
                style={{ color: 'var(--pc-text-secondary)' }}
              >
                {addModalTemplate.description}
              </p>
            )}
            {addError && (
              <div
                className="mb-4 rounded-xl border p-3 text-sm animate-fade-in"
                style={{
                  background: 'rgba(239, 68, 68, 0.08)',
                  borderColor: 'rgba(239, 68, 68, 0.2)',
                  color: '#f87171',
                }}
              >
                {addError}
              </div>
            )}
            <div className="space-y-4">
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: 'var(--pc-text-secondary)' }}
                >
                  {addModalTemplate.kind === 'map' ? 'Key' : 'Identifier'}{' '}
                  <span style={{ color: 'var(--color-status-error)' }}>*</span>
                </label>
                {/* For providers.models specifically, source the picker from
                    the catalog so users don't have to know canonical names. */}
                {addModalTemplate.path === 'providers.models' && catalog.length > 0 ? (
                  <select
                    autoFocus
                    value={addKey}
                    onChange={(e) => setAddKey(e.target.value)}
                    className="input-electric w-full px-3 py-2.5 text-sm appearance-none cursor-pointer"
                  >
                    <option value="">— pick a provider —</option>
                    {catalog.map((p) => (
                      <option key={p.name} value={p.name}>
                        {p.display_name}
                        {p.local ? ' (local)' : ''}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    autoFocus
                    type="text"
                    value={addKey}
                    onChange={(e) => setAddKey(e.target.value)}
                    placeholder={
                      addModalTemplate.kind === 'map'
                        ? 'e.g. anthropic'
                        : 'e.g. my-server'
                    }
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') void handleAdd();
                    }}
                    className="input-electric w-full px-3 py-2.5 text-sm"
                  />
                )}
              </div>
            </div>
            <div className="flex justify-end gap-3 mt-6">
              <button
                onClick={() => {
                  setAddModalTemplate(null);
                  setAddKey('');
                  setAddError(null);
                }}
                className="btn-secondary px-4 py-2 text-sm font-medium"
              >
                Cancel
              </button>
              <button
                onClick={handleAdd}
                disabled={!addKey.trim() || adding}
                className="btn-electric px-4 py-2 text-sm font-medium"
              >
                {adding ? 'Adding…' : 'Add'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Field groups, one per category. */}
      <div className="flex flex-col gap-4 flex-1 min-h-0">
        {groups.map((g) => (
          <section
            key={g.category}
            className="surface-panel overflow-hidden"
          >
            <h3
              className="px-4 py-2.5 text-xs font-semibold uppercase tracking-wider border-b"
              style={{
                color: 'var(--pc-text-secondary)',
                background: 'var(--pc-bg-elevated)',
                borderColor: 'var(--pc-border)',
              }}
            >
              {g.category}
            </h3>
            <div
              className="divide-y"
              style={{ borderColor: 'var(--pc-border)' }}
            >
              {g.entries.map((f) => (
                <FieldRow
                  key={f.path}
                  entry={f}
                  value={draft[f.path] ?? ''}
                  onChange={(v) => setDraft((d) => ({ ...d, [f.path]: v }))}
                  comment={comments[f.path] ?? ''}
                  onCommentChange={(v) => setComments((c) => ({ ...c, [f.path]: v }))}
                  error={fieldErrors[f.path]}
                  onDelete={() => handleDelete(f.path)}
                  onProviderModelsLoad={ensureModelsLoaded}
                />
              ))}
            </div>
          </section>
        ))}
      </div>
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
  onDelete: () => void;
  onProviderModelsLoad: (provider: string) => Promise<string[]>;
}

function FieldRow({
  entry,
  value,
  onChange,
  comment,
  onCommentChange,
  error,
  onDelete,
  onProviderModelsLoad,
}: FieldRowProps) {
  const renderer = rendererFor(entry);
  const [providerModels, setProviderModels] = useState<string[] | null>(null);
  const isProviderModelField = /^providers\.models\.[^.]+\.model$/.test(entry.path);

  useEffect(() => {
    if (!isProviderModelField) return;
    const provider = entry.path.split('.')[2];
    if (!provider) return;
    void onProviderModelsLoad(provider).then(setProviderModels);
  }, [isProviderModelField, entry.path, onProviderModelsLoad]);

  return (
    <div className="px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <div className="flex-1 min-w-0">
          <label
            className="block text-sm font-medium"
            style={{ color: 'var(--pc-text-primary)' }}
            htmlFor={entry.path}
          >
            {fieldLabel(entry)}
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
            className="block text-xs mt-0.5"
            style={{ color: 'var(--pc-text-faint)' }}
          >
            {entry.path}{' '}
            <span style={{ color: 'var(--pc-text-faint)', opacity: 0.6 }}>
              ({entry.type_hint})
            </span>
          </code>
        </div>
        <button
          onClick={onDelete}
          title="Reset to default / unset"
          className="btn-icon flex-shrink-0"
        >
          <Trash2 className="h-4 w-4" />
        </button>
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
        ) : isProviderModelField && providerModels && providerModels.length > 0 ? (
          <>
            <input
              id={entry.path}
              list={`models-${entry.path}`}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="input-electric w-full px-3 py-2 text-sm"
              placeholder={entry.type_hint}
            />
            <datalist id={`models-${entry.path}`}>
              {providerModels.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
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
          <p
            className="mt-1 text-sm"
            style={{ color: 'var(--color-status-error)' }}
          >
            <span className="font-mono text-xs">{error.code}</span>: {error.message}
          </p>
        )}
      </div>
    </div>
  );
}
