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
//
// Daemon restart button hits `POST /admin/restart` and polls `/health` until
// the new instance is ready.

import { useEffect, useMemo, useState } from 'react';
import { Plus, RotateCw, Save, Trash2 } from 'lucide-react';
import {
  ApiError,
  createMapKey,
  deleteProp,
  getCatalog,
  getCatalogModels,
  getTemplates,
  listProps,
  patchConfig,
  restartDaemon,
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

/** Cache of `provider name → models` so we don't refetch on every render. */
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
  const [addingTo, setAddingTo] = useState<string | null>(null);
  const [addKey, setAddKey] = useState('');
  const [restartState, setRestartState] = useState<'idle' | 'restarting' | 'waiting' | 'back'>('idle');

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

    // Build PATCH ops only for fields whose draft differs from the current
    // display value. Skip secret fields with empty input (means "leave it").
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

  const handleAdd = async (template: TemplateEntry) => {
    const key = addKey.trim();
    if (!key) return;
    try {
      await createMapKey(template.path, key);
      setAddingTo(null);
      setAddKey('');
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

  const handleRestart = async () => {
    if (!confirm('Restart the daemon? Connections will briefly drop until the new instance is ready.')) return;
    setRestartState('restarting');
    try {
      await restartDaemon();
      setRestartState('waiting');
      // Poll /health until it answers.
      const start = Date.now();
      while (Date.now() - start < 30_000) {
        await new Promise((r) => setTimeout(r, 800));
        try {
          const r = await fetch('/health');
          if (r.ok) {
            setRestartState('back');
            setTimeout(() => {
              setRestartState('idle');
              void reload();
            }, 1500);
            return;
          }
        } catch {
          // keep polling
        }
      }
      setError('Daemon did not respond within 30s. Check the gateway logs.');
      setRestartState('idle');
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
      setRestartState('idle');
    }
  };

  // Fetch model list for a provider when the user picks it.
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

  if (loading) return <div className="p-6">Loading config…</div>;

  return (
    <div className="flex flex-col gap-4 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Configuration</h1>
          <p className="text-sm text-gray-500">
            Schema-driven editor. Every field comes from the gateway's per-property API.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleRestart}
            disabled={restartState !== 'idle'}
            className="flex items-center gap-2 rounded border px-3 py-2 text-sm disabled:opacity-50"
          >
            <RotateCw className={`h-4 w-4 ${restartState !== 'idle' ? 'animate-spin' : ''}`} />
            {restartState === 'idle'
              ? 'Restart daemon'
              : restartState === 'restarting'
              ? 'Restarting…'
              : restartState === 'waiting'
              ? 'Waiting for daemon…'
              : 'Daemon back ✓'}
          </button>
          <button
            onClick={handleSaveAll}
            disabled={saving}
            className="flex items-center gap-2 rounded bg-blue-600 px-4 py-2 text-sm text-white disabled:opacity-50"
          >
            <Save className="h-4 w-4" />
            {saving ? 'Saving…' : 'Save all changes'}
          </button>
        </div>
      </div>

      {error && (
        <div className="rounded border border-red-300 bg-red-50 p-3 text-sm text-red-700">{error}</div>
      )}
      {savedAt && (
        <div className="rounded border border-green-300 bg-green-50 p-3 text-sm text-green-700">{savedAt}</div>
      )}

      {/* "+ Add" affordances for map-keyed and list-shaped sections — discovered
          from /api/config/templates, which the gateway derives from
          Configurable::map_key_sections(). No hardcoded list anywhere. */}
      {templates.length > 0 && (
        <div className="rounded border border-gray-200 bg-gray-50 p-4">
          <h2 className="mb-2 text-sm font-semibold text-gray-700">Add new entry</h2>
          <div className="flex flex-wrap gap-2">
            {templates.map((t) => (
              <div key={t.path} className="flex flex-col">
                <button
                  onClick={() => setAddingTo(t.path === addingTo ? null : t.path)}
                  className="flex items-center gap-1 rounded border px-2 py-1 text-xs hover:bg-gray-100"
                  title={t.description || `${t.kind}: ${t.value_type}`}
                >
                  <Plus className="h-3 w-3" />
                  {t.path}
                  <span className="ml-1 text-gray-400">[{t.kind}]</span>
                </button>
                {addingTo === t.path && (
                  <div className="mt-1 flex items-center gap-1">
                    {t.path === 'providers.models' && catalog.length > 0 ? (
                      <select
                        value={addKey}
                        onChange={(e) => setAddKey(e.target.value)}
                        className="rounded border px-2 py-1 text-xs"
                      >
                        <option value="">— pick provider —</option>
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
                        value={addKey}
                        onChange={(e) => setAddKey(e.target.value)}
                        placeholder={t.kind === 'map' ? 'name' : 'identifier'}
                        className="rounded border px-2 py-1 text-xs"
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') void handleAdd(t);
                          if (e.key === 'Escape') {
                            setAddingTo(null);
                            setAddKey('');
                          }
                        }}
                      />
                    )}
                    <button
                      onClick={() => void handleAdd(t)}
                      disabled={!addKey.trim()}
                      className="rounded bg-blue-600 px-2 py-1 text-xs text-white disabled:opacity-50"
                    >
                      Add
                    </button>
                  </div>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Field groups, one per category. */}
      {groups.map((g) => (
        <section key={g.category} className="rounded border border-gray-200">
          <h2 className="border-b border-gray-200 bg-gray-50 px-4 py-2 text-sm font-semibold uppercase tracking-wider text-gray-600">
            {g.category}
          </h2>
          <div className="divide-y divide-gray-100">
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

  // For provider model fields, lazy-fetch the catalog when the row mounts.
  useEffect(() => {
    if (!isProviderModelField) return;
    const provider = entry.path.split('.')[2];
    if (!provider) return;
    void onProviderModelsLoad(provider).then(setProviderModels);
  }, [isProviderModelField, entry.path, onProviderModelsLoad]);

  return (
    <div className="px-4 py-3">
      <div className="flex items-start justify-between gap-3">
        <div className="flex-1">
          <label className="block text-sm font-medium" htmlFor={entry.path}>
            {fieldLabel(entry)}
            {entry.is_secret && (
              <span className="ml-2 text-xs text-gray-500">
                🔒 {entry.populated ? 'set' : 'unset'}
              </span>
            )}
          </label>
          <code className="block text-xs text-gray-400">
            {entry.path} <span className="text-gray-300">({entry.type_hint})</span>
          </code>
        </div>
        <button
          onClick={onDelete}
          title="Reset to default / unset"
          className="text-gray-400 hover:text-red-500"
        >
          <Trash2 className="h-4 w-4" />
        </button>
      </div>

      <div className="mt-2">
        {renderer === 'bool' ? (
          <select
            id={entry.path}
            value={value || 'false'}
            onChange={(e) => onChange(e.target.value)}
            className="w-full rounded border px-2 py-1"
          >
            <option value="true">true</option>
            <option value="false">false</option>
          </select>
        ) : renderer === 'select' ? (
          <select
            id={entry.path}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="w-full rounded border px-2 py-1"
          >
            <option value="">—</option>
            {(entry.enum_variants ?? []).map((v) => (
              <option key={v} value={v}>
                {v}
              </option>
            ))}
          </select>
        ) : isProviderModelField && providerModels && providerModels.length > 0 ? (
          // Provider-model-field gets a catalog-backed datalist on top of a
          // text input — pick from the live catalog OR type freely.
          <>
            <input
              id={entry.path}
              list={`models-${entry.path}`}
              value={value}
              onChange={(e) => onChange(e.target.value)}
              className="w-full rounded border px-2 py-1"
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
            className="w-full rounded border px-2 py-1 font-mono text-sm"
            placeholder="One value per line"
          />
        ) : renderer === 'number' ? (
          <input
            id={entry.path}
            type="number"
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="w-full rounded border px-2 py-1"
          />
        ) : (
          <input
            id={entry.path}
            type={renderer === 'secret' ? 'password' : 'text'}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="w-full rounded border px-2 py-1"
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
          className="mt-1 w-full rounded border px-2 py-1 text-xs text-gray-600"
        />

        {error && (
          <p className="mt-1 text-sm text-red-600">
            <span className="font-mono">{error.code}</span>: {error.message}
          </p>
        )}
      </div>
    </div>
  );
}
