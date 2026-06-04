// Providers tab — full model-provider management.
//
// Replaces the old read-only Integrations dashboard. Model providers are the
// `providers.models` config section (a TypedFamilyMap: one or more named
// instances per provider family, e.g. `providers.models.anthropic.default`).
// Everything reuses the existing config backend:
//   - types:      getCatalog()                      → provider families
//   - instances:  listProps('providers.models')     → configured <type>.<alias>
//   - add:        selectSectionItem('providers.models', type, alias)
//   - edit:       <FieldForm prefix=...>  (secrets, model-picker, tabs, save)
//   - delete:     deleteMapKey('providers.models.<type>', alias)

import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Cpu,
  Plus,
  Trash2,
  ChevronDown,
  ChevronRight,
  Loader2,
  Search,
  Server,
  Cloud,
} from 'lucide-react';
import {
  getCatalog,
  getCatalogModels,
  listProps,
  patchConfig,
  selectSectionItem,
  deleteMapKey,
  type CatalogProvider,
  type ListResponseEntry,
} from '@/lib/api';
import FieldForm from '@/components/sections/FieldForm';
import ReloadDaemonButton from '@/components/sections/ReloadDaemonButton';

interface Instance {
  type: string;
  alias: string;
  model: string | null;
}

export default function Providers() {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [catalog, setCatalog] = useState<CatalogProvider[]>([]);
  const [instances, setInstances] = useState<Instance[]>([]);
  const [pendingReload, setPendingReload] = useState(false);

  const displayName = useMemo(() => {
    const m = new Map<string, string>();
    catalog.forEach((p) => m.set(p.name, p.display_name));
    return m;
  }, [catalog]);

  // How many instances are configured per provider family (for the catalog badges).
  const configuredCount = useMemo(() => {
    const m = new Map<string, number>();
    instances.forEach((i) => m.set(i.type, (m.get(i.type) ?? 0) + 1));
    return m;
  }, [instances]);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [cat, props] = await Promise.all([
        getCatalog(),
        listProps('providers.models').catch(() => ({ entries: [] as ListResponseEntry[] })),
      ]);
      setCatalog([...cat.model_providers].sort((a, b) => a.display_name.localeCompare(b.display_name)));

      // Discover configured <type>.<alias> instances from the field paths.
      const byKey = new Map<string, Instance>();
      for (const e of props.entries) {
        const m = /^providers\.models\.([^.]+)\.([^.]+)\.(.+)$/.exec(e.path);
        if (!m) continue;
        const [, type, alias, field] = m;
        const key = `${type}/${alias}`;
        const inst = byKey.get(key) ?? { type: type!, alias: alias!, model: null };
        if (field === 'model' && typeof e.value === 'string' && e.value !== '<unset>') {
          inst.model = e.value;
        }
        byKey.set(key, inst);
      }
      setInstances(
        [...byKey.values()].sort((a, b) =>
          a.type === b.type ? a.alias.localeCompare(b.alias) : a.type.localeCompare(b.type),
        ),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }
  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          Failed to load models: {error}
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center gap-3">
        <Cpu className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
        <div>
          <h1 className="text-lg font-semibold" style={{ color: 'var(--pc-text-primary)' }}>Providers</h1>
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            Connect a model provider, choose its model, and configure API keys and endpoints.
          </p>
        </div>
      </div>

      {pendingReload && (
        <div className="rounded-2xl border p-4 flex items-center justify-between gap-4 flex-wrap" style={{ background: 'var(--pc-accent-glow)', borderColor: 'var(--pc-accent-dim)' }}>
          <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
            Saved. Provider changes take effect after the daemon reloads.
          </p>
          <ReloadDaemonButton onReloaded={() => { setPendingReload(false); void load(); }} />
        </div>
      )}

      <ProviderCatalog
        catalog={catalog}
        configuredCount={configuredCount}
        onAdded={() => { setPendingReload(true); void load(); }}
      />

      <div className="space-y-4">
        <div className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          Configured ({instances.length})
        </div>
        {instances.length === 0 ? (
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            No model providers configured yet. Add one above to get started.
          </p>
        ) : (
          instances.map((inst) => (
            <InstanceCard
              key={`${inst.type}/${inst.alias}`}
              instance={inst}
              displayName={displayName.get(inst.type) ?? inst.type}
              onSaved={() => setPendingReload(true)}
              onDeleted={() => { setPendingReload(true); void load(); }}
            />
          ))
        )}
      </div>
    </div>
  );
}

// Browsable catalog of ALL model providers (not a hidden dropdown) — searchable,
// each with a configured-instance count and an inline "add instance" affordance.
function ProviderCatalog({
  catalog,
  configuredCount,
  onAdded,
}: {
  catalog: CatalogProvider[];
  configuredCount: Map<string, number>;
  onAdded: () => void;
}) {
  const [search, setSearch] = useState('');
  const [openType, setOpenType] = useState<string | null>(null);

  const q = search.trim().toLowerCase();
  const filtered = q
    ? catalog.filter((p) => p.display_name.toLowerCase().includes(q) || p.name.toLowerCase().includes(q))
    : catalog;

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <div className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          Add a provider ({catalog.length})
        </div>
        <div className="relative w-full sm:w-72">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4" style={{ color: 'var(--pc-text-faint)' }} />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search providers…"
            className="input-electric w-full pl-9 pr-3 py-2 text-sm"
          />
        </div>
      </div>

      {filtered.length === 0 ? (
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>No providers match “{search}”.</p>
      ) : (
        <div className="grid grid-cols-1 sm:grid-cols-2 xl:grid-cols-3 gap-2 max-h-[28rem] overflow-y-auto pr-1">
          {filtered.map((p) => (
            <ProviderTile
              key={p.name}
              provider={p}
              configured={configuredCount.get(p.name) ?? 0}
              open={openType === p.name}
              onToggle={() => setOpenType((cur) => (cur === p.name ? null : p.name))}
              onAdded={() => { setOpenType(null); onAdded(); }}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function ProviderTile({
  provider,
  configured,
  open,
  onToggle,
  onAdded,
}: {
  provider: CatalogProvider;
  configured: number;
  open: boolean;
  onToggle: () => void;
  onAdded: () => void;
}) {
  const [alias, setAlias] = useState('default');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const add = async () => {
    setBusy(true);
    setErr(null);
    try {
      await selectSectionItem('providers.models', provider.name, alias.trim() || 'default');
      setAlias('default');
      onAdded();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="rounded-xl border transition-colors"
      style={{ borderColor: open ? 'var(--pc-accent-dim)' : 'var(--pc-border)', background: open ? 'var(--pc-accent-glow)' : 'transparent' }}
    >
      <button type="button" onClick={onToggle} className="w-full flex items-center justify-between gap-2 px-3 py-2.5 text-left" style={{ background: 'transparent' }}>
        <span className="flex items-center gap-2 min-w-0">
          {provider.local
            ? <Server className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-muted)' }} />
            : <Cloud className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-muted)' }} />}
          <span className="text-sm font-medium truncate" style={{ color: 'var(--pc-text-primary)' }}>{provider.display_name}</span>
          {provider.local && (
            <span className="text-[9px] uppercase tracking-wider px-1 py-0.5 rounded" style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-faint)' }}>local</span>
          )}
        </span>
        {configured > 0 && (
          <span className="text-[10px] font-semibold px-1.5 py-0.5 rounded-full border shrink-0" style={{ borderColor: 'var(--pc-accent-dim)', color: 'var(--pc-accent-light)' }}>
            {configured}
          </span>
        )}
      </button>

      {open && (
        <div className="px-3 pb-3 pt-1 flex items-end gap-2">
          <div className="flex-1">
            <label className="block text-[10px] font-semibold uppercase tracking-wider mb-1" style={{ color: 'var(--pc-text-muted)' }}>Instance name</label>
            <input type="text" value={alias} onChange={(e) => setAlias(e.target.value)} placeholder="default"
              className="input-electric w-full px-2.5 py-1.5 text-sm" />
          </div>
          <button type="button" onClick={() => void add()} disabled={busy} className="btn-electric inline-flex items-center gap-1.5 text-sm px-3 py-1.5">
            {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <Plus className="h-4 w-4" />} Add
          </button>
        </div>
      )}
      {err && <p className="px-3 pb-2 text-xs" style={{ color: '#f87171' }}>{err}</p>}
    </div>
  );
}

function InstanceCard({
  instance,
  displayName,
  onSaved,
  onDeleted,
}: {
  instance: Instance;
  displayName: string;
  onSaved: () => void;
  onDeleted: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // Local model mirror so the header badge updates after a pick without a full
  // page reload (which would collapse the expanded card).
  const [model, setModel] = useState(instance.model);

  const remove = async () => {
    setDeleting(true);
    try {
      await deleteMapKey(`providers.models.${instance.type}`, instance.alias);
      onDeleted();
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div className="card overflow-hidden">
      <div className="p-4 flex items-center justify-between gap-3">
        <button type="button" onClick={() => setOpen((v) => !v)} className="flex items-center gap-2 min-w-0 text-left" style={{ background: 'transparent' }}>
          {open ? <ChevronDown className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-accent)' }} /> : <ChevronRight className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-faint)' }} />}
          <Cpu className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-accent)' }} />
          <span className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>{displayName}</span>
          <span className="text-xs px-1.5 py-0.5 rounded font-mono" style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-muted)' }}>{instance.alias}</span>
          {model && (
            <span className="text-xs font-mono truncate" style={{ color: 'var(--pc-text-faint)' }}>{model}</span>
          )}
        </button>
        <button type="button" onClick={() => void remove()} disabled={deleting} className="btn-icon" aria-label="Delete provider">
          {deleting ? <Loader2 className="h-4 w-4 animate-spin" /> : <Trash2 className="h-4 w-4" style={{ color: '#f87171' }} />}
        </button>
      </div>

      {open && (
        <div className="border-t p-4 space-y-4" style={{ borderColor: 'var(--pc-border)' }}>
          {/* Elevated model picker — populated from the provider's live catalog. */}
          <ModelPicker
            type={instance.type}
            alias={instance.alias}
            currentModel={model}
            onChange={(m) => { setModel(m); onSaved(); }}
          />
          {/* The rest of the provider config (API key, endpoint, advanced) reuses
              the canonical editor. The `.model` field is hidden here since the
              picker above owns it. */}
          <FieldForm
            prefix={`providers.models.${instance.type}.${instance.alias}`}
            onSaved={onSaved}
            includePath={(p) => !/\.model$/.test(p)}
            inlineSaveBar
          />
        </div>
      )}
    </div>
  );
}

// Prominent model selector driven by the provider's live catalog
// (`/api/config/catalog/models`). Falls back to free-text when the upstream
// catalog is unavailable. Saves the choice immediately via patchConfig.
function ModelPicker({
  type,
  alias,
  currentModel,
  onChange,
}: {
  type: string;
  alias: string;
  currentModel: string | null;
  onChange: (model: string) => void;
}) {
  const [models, setModels] = useState<string[]>([]);
  const [live, setLive] = useState(true);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [custom, setCustom] = useState(false);
  const [text, setText] = useState(currentModel ?? '');

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getCatalogModels(type, alias)
      .then((r) => { if (!cancelled) { setModels(r.models); setLive(r.live); } })
      .catch(() => { if (!cancelled) setLive(false); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [type, alias]);

  const save = async (m: string) => {
    setSaving(true);
    try {
      await patchConfig([{ op: 'replace', path: `providers.models.${type}.${alias}.model`, value: m }]);
      onChange(m);
    } finally {
      setSaving(false);
    }
  };

  const hasList = live && models.length > 0;
  // Ensure the current model is selectable even if it's not in the catalog.
  const options = currentModel && !models.includes(currentModel) ? [currentModel, ...models] : models;
  const showText = custom || !hasList;

  return (
    <div>
      <div className="flex items-center gap-2 mb-1.5">
        <label className="text-[11px] font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-muted)' }}>Model</label>
        {saving && <Loader2 className="h-3 w-3 animate-spin" style={{ color: 'var(--pc-accent)' }} />}
        {!loading && !live && <span className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>catalog unavailable — type any model id</span>}
      </div>

      {loading ? (
        <div className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>Loading models…</div>
      ) : showText ? (
        <div className="flex items-center gap-2">
          <input
            type="text"
            list={`models-${type}-${alias}`}
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="e.g. gpt-4o"
            className="input-electric flex-1 px-3 py-2 text-sm font-mono"
          />
          <datalist id={`models-${type}-${alias}`}>
            {models.map((m) => <option key={m} value={m} />)}
          </datalist>
          <button type="button" onClick={() => void save(text.trim())} disabled={saving || !text.trim()} className="btn-electric text-sm px-3 py-2">Set</button>
          {hasList && (
            <button type="button" onClick={() => setCustom(false)} className="btn-secondary text-sm px-3 py-2">List</button>
          )}
        </div>
      ) : (
        <select
          value={currentModel ?? ''}
          onChange={(e) => {
            if (e.target.value === '__custom__') { setCustom(true); setText(currentModel ?? ''); return; }
            void save(e.target.value);
          }}
          className="input-electric w-full px-3 py-2 text-sm"
        >
          {!currentModel && <option value="" disabled>Select a model…</option>}
          {options.map((m) => <option key={m} value={m}>{m}</option>)}
          <option value="__custom__">✏️ Custom model…</option>
        </select>
      )}
    </div>
  );
}
