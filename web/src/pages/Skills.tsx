// Dedicated Skills tab.
//
// Surfaces the existing skills backend (api_skills.rs + the `skills` /
// `skill_bundles` config sections) as a first-class page:
//  - Global skills settings (read generically from `listProps('skills')`,
//    written via `patchConfig` — same contract as the MCP toggles).
//  - Bundle management: create/delete bundles, edit directory + include/exclude
//    (a `skill_bundles.<alias>.*` Map section, so fields ARE patchable).
//  - Per-bundle skill CRUD via the existing <SkillsBundleEditor/> drill-in.

import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Wand2,
  Plus,
  Trash2,
  Save,
  ChevronDown,
  ChevronRight,
  Loader2,
  FolderTree,
} from 'lucide-react';
import {
  listProps,
  patchConfig,
  listSkillBundles,
  createMapKey,
  deleteMapKey,
  type ListResponseEntry,
  type PatchOp,
  type SkillBundleEntry,
} from '@/lib/api';
import SkillsBundleEditor from '@/components/sections/SkillsBundleEditor';
import ReloadDaemonButton from '@/components/sections/ReloadDaemonButton';

const UNSET = '<unset>';

export default function Skills() {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [settings, setSettings] = useState<ListResponseEntry[]>([]);
  const [bundles, setBundles] = useState<SkillBundleEntry[]>([]);
  const [pendingReload, setPendingReload] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [props, bundleResp] = await Promise.all([
        listProps('skills').catch(() => ({ entries: [] as ListResponseEntry[] })),
        listSkillBundles(),
      ]);
      // Only the top-level scalar settings (skip nested skill_creation.* etc.).
      const topLevel = props.entries.filter(
        (e) =>
          /^skills\.[^.]+$/.test(e.path) &&
          ['bool', 'enum', 'string', 'integer', 'float'].includes(e.kind),
      );
      setSettings(topLevel);
      setBundles(bundleResp.bundles);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

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
          Failed to load skills: {error}
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center gap-3">
        <Wand2 className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
        <div>
          <h1 className="text-lg font-semibold" style={{ color: 'var(--pc-text-primary)' }}>Skills</h1>
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            Manage skill bundles, edit individual skills, and configure how skills load.
          </p>
        </div>
      </div>

      {pendingReload && (
        <div className="rounded-2xl border p-4 flex items-center justify-between gap-4 flex-wrap" style={{ background: 'var(--pc-accent-glow)', borderColor: 'var(--pc-accent-dim)' }}>
          <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
            Saved. Some skill changes apply on the next daemon reload.
          </p>
          <ReloadDaemonButton onReloaded={() => { setPendingReload(false); void load(); }} />
        </div>
      )}

      <SettingsPanel entries={settings} onSaved={() => setPendingReload(true)} />

      <BundlesSection
        bundles={bundles}
        onChanged={() => { setPendingReload(true); void load(); }}
      />
    </div>
  );
}

// ── Global skills settings (generic prop renderer) ───────────────────────────

function SettingsPanel({ entries, onSaved }: { entries: ListResponseEntry[]; onSaved: () => void }) {
  const initial = useMemo(() => {
    const m: Record<string, string> = {};
    for (const e of entries) {
      const v = typeof e.value === 'string' ? e.value : '';
      m[e.path] = v === UNSET ? '' : v;
    }
    return m;
  }, [entries]);

  const [draft, setDraft] = useState<Record<string, string>>(initial);
  // Baseline tracks the last-saved values so the Save button disables again
  // after a successful save (without a full page reload).
  const [baseline, setBaseline] = useState<Record<string, string>>(initial);
  const [saving, setSaving] = useState(false);
  useEffect(() => { setDraft(initial); setBaseline(initial); }, [initial]);

  if (entries.length === 0) return null;

  const dirty = entries.some((e) => (draft[e.path] ?? '') !== (baseline[e.path] ?? ''));

  const save = async () => {
    setSaving(true);
    try {
      const ops: PatchOp[] = [];
      for (const e of entries) {
        const next = draft[e.path] ?? '';
        if (next === (baseline[e.path] ?? '')) continue;
        let value: unknown = next;
        if (e.kind === 'bool') value = next === 'true';
        else if (e.kind === 'integer' || e.kind === 'float') value = next === '' ? null : Number(next);
        else if (next === '') value = null;
        ops.push({ op: 'replace', path: e.path, value });
      }
      if (ops.length) {
        await patchConfig(ops);
        setBaseline(draft);
        onSaved();
      }
    } finally {
      setSaving(false);
    }
  };

  const label = (path: string) => path.replace(/^skills\./, '').replace(/_/g, ' ');

  return (
    <div className="card p-4 space-y-3">
      <div className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
        Settings
      </div>
      {entries.map((e) => (
        <div key={e.path} className="flex items-center justify-between gap-4">
          <label htmlFor={e.path} className="text-sm capitalize" style={{ color: 'var(--pc-text-secondary)' }}>{label(e.path)}</label>
          {e.kind === 'bool' ? (
            <input
              id={e.path}
              type="checkbox"
              checked={(draft[e.path] ?? 'false') === 'true'}
              onChange={(ev) => setDraft((d) => ({ ...d, [e.path]: ev.target.checked ? 'true' : 'false' }))}
              className="h-5 w-9 shrink-0 appearance-none rounded-full transition-colors cursor-pointer"
              style={{ background: (draft[e.path] ?? 'false') === 'true' ? 'var(--pc-accent)' : 'var(--pc-border)' }}
            />
          ) : e.kind === 'enum' ? (
            <select
              id={e.path}
              value={draft[e.path] ?? ''}
              onChange={(ev) => setDraft((d) => ({ ...d, [e.path]: ev.target.value }))}
              className="input-electric px-3 py-1.5 text-sm w-56"
            >
              {(e.enum_variants ?? []).map((v) => <option key={v} value={v}>{v}</option>)}
            </select>
          ) : (
            <input
              id={e.path}
              type={e.kind === 'integer' || e.kind === 'float' ? 'number' : 'text'}
              value={draft[e.path] ?? ''}
              onChange={(ev) => setDraft((d) => ({ ...d, [e.path]: ev.target.value }))}
              className="input-electric px-3 py-1.5 text-sm w-56"
            />
          )}
        </div>
      ))}
      <div className="flex justify-end">
        <button type="button" onClick={() => void save()} disabled={!dirty || saving}
          className="btn-electric inline-flex items-center gap-1.5 text-sm px-3 py-2">
          {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />} Save settings
        </button>
      </div>
    </div>
  );
}

// ── Bundles ──────────────────────────────────────────────────────────────────

function BundlesSection({ bundles, onChanged }: { bundles: SkillBundleEntry[]; onChanged: () => void }) {
  const [newAlias, setNewAlias] = useState('');
  const [adding, setAdding] = useState(false);

  const addBundle = async () => {
    const alias = newAlias.trim();
    if (!alias) return;
    setAdding(true);
    try {
      await createMapKey('skill_bundles', alias);
      setNewAlias('');
      onChanged();
    } finally {
      setAdding(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
        Bundles ({bundles.length})
      </div>

      {bundles.length === 0 ? (
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          No skill bundles yet. Create one below to group skills for your agents.
        </p>
      ) : (
        bundles.map((b) => <BundleCard key={b.alias} bundle={b} onChanged={onChanged} />)
      )}

      <div className="flex items-center gap-2">
        <input type="text" value={newAlias} onChange={(e) => setNewAlias(e.target.value)}
          placeholder="new-bundle-name" className="input-electric px-3 py-2 text-sm" />
        <button type="button" onClick={() => void addBundle()} disabled={adding || !newAlias.trim()}
          className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2">
          {adding ? <Loader2 className="h-4 w-4 animate-spin" /> : <Plus className="h-4 w-4" />} Add bundle
        </button>
      </div>
    </div>
  );
}

function BundleCard({ bundle, onChanged }: { bundle: SkillBundleEntry; onChanged: () => void }) {
  const [open, setOpen] = useState(false);
  const [directory, setDirectory] = useState(bundle.directory ?? '');
  const [include, setInclude] = useState((bundle.include ?? []).join(', '));
  const [exclude, setExclude] = useState((bundle.exclude ?? []).join(', '));
  const [busy, setBusy] = useState<'save' | 'delete' | null>(null);

  const parseList = (s: string) => s.split(',').map((x) => x.trim()).filter(Boolean);

  const save = async () => {
    setBusy('save');
    try {
      await patchConfig([
        { op: 'replace', path: `skill_bundles.${bundle.alias}.directory`, value: directory.trim() === '' ? null : directory.trim() },
        { op: 'replace', path: `skill_bundles.${bundle.alias}.include`, value: parseList(include) },
        { op: 'replace', path: `skill_bundles.${bundle.alias}.exclude`, value: parseList(exclude) },
      ]);
      onChanged();
    } finally {
      setBusy(null);
    }
  };

  const remove = async () => {
    setBusy('delete');
    try {
      await deleteMapKey('skill_bundles', bundle.alias);
      onChanged();
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="card overflow-hidden">
      <div className="p-4 flex items-center justify-between gap-3">
        <button type="button" onClick={() => setOpen((v) => !v)} className="flex items-center gap-2 min-w-0 text-left" style={{ background: 'transparent' }}>
          {open ? <ChevronDown className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-accent)' }} /> : <ChevronRight className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-faint)' }} />}
          <FolderTree className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-accent)' }} />
          <span className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>{bundle.alias}</span>
          <span className="text-xs truncate font-mono" style={{ color: 'var(--pc-text-faint)' }}>{bundle.directory}</span>
        </button>
        <button type="button" onClick={() => void remove()} disabled={busy !== null} className="btn-icon" aria-label="Delete bundle">
          {busy === 'delete' ? <Loader2 className="h-4 w-4 animate-spin" /> : <Trash2 className="h-4 w-4" style={{ color: '#f87171' }} />}
        </button>
      </div>

      {open && (
        <div className="border-t p-4 space-y-4" style={{ borderColor: 'var(--pc-border)' }}>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
            <div>
              <label className="block text-[11px] font-semibold uppercase tracking-wider mb-1.5" style={{ color: 'var(--pc-text-muted)' }}>Directory (under shared/)</label>
              <input type="text" value={directory} onChange={(e) => setDirectory(e.target.value)} placeholder="(default)" className="input-electric w-full px-3 py-2 text-sm font-mono" />
            </div>
            <div>
              <label className="block text-[11px] font-semibold uppercase tracking-wider mb-1.5" style={{ color: 'var(--pc-text-muted)' }}>Include (comma-sep, empty = all)</label>
              <input type="text" value={include} onChange={(e) => setInclude(e.target.value)} className="input-electric w-full px-3 py-2 text-sm" />
            </div>
            <div>
              <label className="block text-[11px] font-semibold uppercase tracking-wider mb-1.5" style={{ color: 'var(--pc-text-muted)' }}>Exclude (comma-sep)</label>
              <input type="text" value={exclude} onChange={(e) => setExclude(e.target.value)} className="input-electric w-full px-3 py-2 text-sm" />
            </div>
          </div>
          <div className="flex justify-end">
            <button type="button" onClick={() => void save()} disabled={busy !== null} className="btn-electric inline-flex items-center gap-1.5 text-sm px-3 py-2">
              {busy === 'save' ? <Loader2 className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />} Save bundle
            </button>
          </div>

          {/* Per-skill CRUD reuses the existing drill-in editor. */}
          <div className="border-t pt-4" style={{ borderColor: 'var(--pc-border)' }}>
            <SkillsBundleEditor bundle={bundle.alias} />
          </div>
        </div>
      )}
    </div>
  );
}
