// Schema-driven onboarding wizard mirroring `zeroclaw onboard` (#6175).
//
// Layout:
//   ┌─ Sidebar ────┐ ┌─ Breadcrumb (Onboard › Section › ?picked) ─┐
//   │ Workspace ✓  │ │ Help text                                   │
//   │ Providers ▶  │ │                                             │
//   │ Channels     │ │  Either: <SectionPicker> (catalog list)     │
//   │ Memory       │ │     Or:  <FieldForm>     (the picked item)  │
//   │ Hardware     │ │                                             │
//   │ Tunnel       │ │  [ Back ]              [ Done — next ▶ ]    │
//   └──────────────┘ └─────────────────────────────────────────────┘
//
// Section list comes from /api/onboard/sections (single source of truth).
// Picker items come from /api/onboard/sections/<key>. Picking POSTs
// /api/onboard/sections/<key>/items/<picked> which instantiates the entry
// and returns the dotted prefix to render fields under. FieldForm reads
// /api/config/list?prefix=<that> and PATCHes on save. Provider model
// fields auto-fetch /api/onboard/catalog/models for the datalist.

import { Suspense, lazy, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { Check, ChevronRight, Plus } from 'lucide-react';
import {
  ApiError,
  getMapKeys,
  getProp,
  getSections,
  patchConfig,
  reloadDaemon,
  selectSectionItem,
  type PickerItem,
  type SectionInfo,
} from '../../lib/api';
import FieldForm, { type FieldFormHandle } from '../../components/onboard/FieldForm';
import SectionPicker from '../../components/onboard/SectionPicker';

// Personality pulls in CodeMirror + markdown rendering (~270KB gzipped).
// Lazy-load so the cost isn't paid until the user actually opens that
// section. Other onboard sections stay synchronous.
const PersonalityEditor = lazy(
  () => import('../../components/onboard/PersonalityEditor'),
);

// Note: prefix is `onboard_state` (verbatim) and the field becomes
// `completed-sections` (snake → kebab via the macro). Matches what
// `Config::prop_fields()` actually emits — fully-kebab `onboard-state.*`
// is wrong and produces `path_not_found` from set_prop.
const COMPLETED_SECTIONS_PATH = 'onboard_state.completed-sections';

// Wizard sections in TUI order (`zeroclaw onboard`'s `Section::as_path_prefix`
// dispatch in `crates/zeroclaw-runtime/src/onboard/mod.rs`). The dashboard
// wizard mirrors the CLI/TUI flow exactly — only these 6 sections, walked
// in this order. The Config explorer at `/config` and the per-section
// editors at `/setup/<section>` are the surfaces for everything else;
// `/onboard` stays a focused setup-completion flow.
const ONBOARD_SECTION_ORDER = [
  'workspace',
  'providers',
  'channels',
  'memory',
  'hardware',
  'tunnel',
  // Personality is intentionally last — the structural sections above
  // (workspace, providers, memory, …) are answered first so the markdown
  // files the user authors here can reference whatever was just
  // configured. Mirrors the CLI/TUI run_all() loop.
  'personality',
] as const;

// Sections handled by a dedicated component instead of the schema-driven
// FieldForm. The gateway's /api/onboard/sections doesn't enumerate
// these — they're synthesized client-side and slotted into the same
// sidebar/breadcrumb/Next/Finish flow as the schema-backed sections.
const SYNTHETIC_SECTIONS: Record<string, SectionInfo> = {
  personality: {
    key: 'personality',
    label: 'Personality',
    help: 'Edit the markdown files that shape your agent — SOUL, IDENTITY, USER, etc.',
    has_picker: false,
    completed: false,
    group: 'Onboarding',
  },
};

export default function Onboard() {
  const navigate = useNavigate();
  const [sections, setSections] = useState<SectionInfo[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [picked, setPicked] = useState<{ item: PickerItem; fieldsPrefix: string } | null>(null);
  // When a provider/channel type is selected, show alias list inline before opening form.
  const [pickedType, setPickedType] = useState<{ item: PickerItem; sectionKey: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [finishing, setFinishing] = useState(false);
  const [advancing, setAdvancing] = useState(false);
  // Ref into the currently-rendered FieldForm (direct-form sections like
  // Workspace, or the post-pick form for Providers/Channels/Tunnel) so
  // breadcrumb Next/Finish can flush unsaved edits before advancing.
  const formRef = useRef<FieldFormHandle | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getSections()
      .then((resp) => {
        if (cancelled) return;
        // Mirror the TUI flow: only the 6 onboarding sections, in their
        // canonical order. The gateway returns every top-level config
        // section now (#6175 schema-driven discovery) — we filter +
        // re-order here to keep `/onboard` focused on setup completion.
        const byKey = new Map(resp.sections.map((s) => [s.key, s] as const));
        const ordered = ONBOARD_SECTION_ORDER.flatMap((k) => {
          const s = byKey.get(k) ?? SYNTHETIC_SECTIONS[k];
          return s ? [s] : [];
        });
        setSections(ordered);
        // Open the first not-yet-completed section.
        const next = ordered.find((s) => !s.completed);
        setActiveKey(next?.key ?? ordered[0]?.key ?? null);
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof ApiError) {
          setError(`[${e.envelope.code}] ${e.envelope.message}`);
        } else {
          setError(`Couldn't load sections: ${e instanceof Error ? e.message : String(e)}`);
        }
      })
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, []);

  const activeSection = useMemo(
    () => sections.find((s) => s.key === activeKey) ?? null,
    [sections, activeKey],
  );

  const goToSection = (key: string) => {
    setActiveKey(key);
    setPicked(null);
    setPickedType(null);
  };

  const openWithAlias = async (item: PickerItem, sectionKey: string, alias: string) => {
    try {
      const resp = await selectSectionItem(sectionKey, item.key, alias);
      setPickedType(null);
      setPicked({ item, fieldsPrefix: resp.fields_prefix });
    } catch (e) {
      if (e instanceof ApiError) {
        setError(`Couldn't open ${item.label}: [${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(`Couldn't open ${item.label}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  };

  const handlePick = async (item: PickerItem) => {
    if (!activeSection) return;
    if (activeSection.key === 'providers' || activeSection.key === 'channels') {
      setPickedType({ item, sectionKey: activeSection.key });
      return;
    }
    try {
      const resp = await selectSectionItem(activeSection.key, item.key);
      setPicked({ item, fieldsPrefix: resp.fields_prefix });
    } catch (e) {
      if (e instanceof ApiError) {
        setError(`Couldn't open ${item.label}: [${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(`Couldn't open ${item.label}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  };

  // Save any pending form edits first; refuse to advance if the save
  // failed (validator rejected something), so the user can fix it.
  const flushActiveForm = async (): Promise<boolean> => {
    if (!formRef.current) return true;
    try {
      return await formRef.current.flushSave();
    } catch {
      return false;
    }
  };

  const advanceSection = async () => {
    if (!activeSection) return;
    setAdvancing(true);
    try {
      if (!(await flushActiveForm())) return;
      // Mark current section completed server-side, then jump to the next.
      try {
        const current = await getProp(COMPLETED_SECTIONS_PATH).catch(() => ({ value: '[]' }));
        const existing = parseCompleted(current.value);
        if (!existing.includes(activeSection.key)) existing.push(activeSection.key);
        await patchConfig([
          { op: 'replace', path: COMPLETED_SECTIONS_PATH, value: existing },
        ]);
        setSections((prev) =>
          prev.map((s) =>
            s.key === activeSection.key ? { ...s, completed: true } : s,
          ),
        );
      } catch (e) {
        // Don't fail the flow on a marker failure — log and proceed.
        // eslint-disable-next-line no-console
        console.warn('Failed to persist completion marker:', e);
      }
      const idx = sections.findIndex((s) => s.key === activeSection.key);
      const next = sections[idx + 1];
      if (next) {
        setActiveKey(next.key);
        setPicked(null);
        setPickedType(null);
      } else {
        setPicked(null);
        setPickedType(null);
      }
    } finally {
      setAdvancing(false);
    }
  };

  // Finish: save the current form (if any), mark the active section
  // completed, reload the daemon so any newly-configured channels /
  // providers / tunnels actually start, then drop the user on the
  // dashboard. Available at every section, not just the last — users
  // can bail early if they don't care to walk the whole flow.
  const finishOnboarding = async () => {
    if (!activeSection) return;
    setFinishing(true);
    try {
      if (!(await flushActiveForm())) return;
      try {
        const current = await getProp(COMPLETED_SECTIONS_PATH).catch(() => ({ value: '[]' }));
        const existing = parseCompleted(current.value);
        if (!existing.includes(activeSection.key)) existing.push(activeSection.key);
        await patchConfig([
          { op: 'replace', path: COMPLETED_SECTIONS_PATH, value: existing },
        ]);
      } catch (e) {
        // eslint-disable-next-line no-console
        console.warn('Failed to persist completion marker on finish:', e);
      }
      try {
        await reloadDaemon();
        await new Promise((r) => setTimeout(r, 400));
      } catch (e) {
        // eslint-disable-next-line no-console
        console.warn('Daemon reload failed after onboarding; user can retry from /config:', e);
      }
      navigate('/');
    } finally {
      setFinishing(false);
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

  if (error) {
    return (
      <div className="p-6">
        <div
          className="rounded-xl border p-4 text-sm"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {error}
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full overflow-hidden">
      {/* Sidebar */}
      <aside
        className="w-56 flex-shrink-0 border-r overflow-y-auto"
        style={{
          borderColor: 'var(--pc-border)',
          background: 'var(--pc-bg-surface)',
        }}
      >
        <div
          className="px-4 py-3 text-xs font-semibold uppercase tracking-wider"
          style={{ color: 'var(--pc-text-secondary)' }}
        >
          Sections
        </div>
        <nav className="flex flex-col">
          {sections.map((s) => (
            <button
              key={s.key}
              type="button"
              onClick={() => goToSection(s.key)}
              className="flex items-center justify-between gap-2 px-4 py-2.5 text-sm text-left transition-colors"
              style={{
                background:
                  s.key === activeKey ? 'var(--pc-accent-glow)' : 'transparent',
                color:
                  s.key === activeKey
                    ? 'var(--pc-accent)'
                    : 'var(--pc-text-primary)',
                fontWeight: s.key === activeKey ? 600 : 400,
                borderLeft:
                  s.key === activeKey
                    ? '2px solid var(--pc-accent)'
                    : '2px solid transparent',
              }}
            >
              <span className="flex items-center gap-2">
                {s.completed && (
                  <Check
                    className="h-3.5 w-3.5"
                    style={{ color: 'var(--color-status-success)' }}
                  />
                )}
                {s.label}
              </span>
              {s.key === activeKey && <ChevronRight className="h-3.5 w-3.5" />}
            </button>
          ))}
        </nav>
      </aside>

      {/* Main pane */}
      <main className="flex-1 overflow-y-auto p-6">
        {activeSection && (
          <div className="flex flex-col gap-4 max-w-3xl">
            {/* Breadcrumb + always-available Next/Done. The form's own Save
                bar advances the flow on save, but users editing nothing
                (Hardware defaults, e.g.) still need a way out — this gives
                them one regardless of dirty state. */}
            <div className="flex items-center justify-between gap-3 flex-wrap">
              <div
                className="text-sm flex items-center gap-1.5 flex-wrap"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                <span style={{ color: 'var(--pc-text-secondary)' }}>Onboard</span>
                <ChevronRight className="h-3 w-3" />
                <span
                  style={{
                    color: picked || pickedType ? 'var(--pc-text-secondary)' : 'var(--pc-accent)',
                    cursor: picked || pickedType ? 'pointer' : 'default',
                    fontWeight: picked || pickedType ? 400 : 600,
                  }}
                  onClick={() => { setPicked(null); setPickedType(null); }}
                >
                  {activeSection.label}
                </span>
                {pickedType && (
                  <>
                    <ChevronRight className="h-3 w-3" />
                    <span style={{ color: 'var(--pc-accent)', fontWeight: 600 }}>
                      {pickedType.item.label}
                    </span>
                  </>
                )}
                {picked && (
                  <>
                    <ChevronRight className="h-3 w-3" />
                    <span style={{ color: 'var(--pc-text-secondary)', cursor: 'pointer' }}
                      onClick={(e) => { e.stopPropagation(); setPickedType({ item: picked.item, sectionKey: activeSection.key }); setPicked(null); }}>
                      {picked.item.label}
                    </span>
                    <ChevronRight className="h-3 w-3" />
                    <span style={{ color: 'var(--pc-accent)', fontWeight: 600 }}>
                      {picked.fieldsPrefix.split('.').at(-1)}
                    </span>
                  </>
                )}
              </div>
              <div className="flex items-center gap-2 flex-shrink-0">
                {/* Finish is available at every section so users can exit
                    early — saves the current form (if any), reloads the
                    daemon, then redirects to /. Next advances to the next
                    section, also save-aware. On the last section Next is
                    redundant with Finish and is hidden. */}
                <button
                  type="button"
                  disabled={finishing || advancing}
                  onClick={() => void finishOnboarding()}
                  className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2"
                  title="Save, reload the daemon, and exit to the dashboard"
                >
                  {finishing ? 'Finishing…' : 'Finish'}
                </button>
                {!isLastSection(sections, activeSection.key) && (
                  <button
                    type="button"
                    disabled={finishing || advancing}
                    onClick={() => void advanceSection()}
                    className="btn-electric inline-flex items-center gap-1.5 text-sm px-4 py-2"
                    title="Save and move to the next section"
                  >
                    {advancing ? 'Saving…' : 'Next ▶'}
                  </button>
                )}
              </div>
            </div>

            {/* Picker view OR form view. Direct-form sections (Workspace,
                Hardware) skip the picker entirely. */}
            {activeSection.key === 'personality' ? (
              <Suspense fallback={<EditorLoading />}>
                <PersonalityEditor />
              </Suspense>
            ) : !activeSection.has_picker ? (
              <FieldForm
                ref={formRef}
                prefix={activeSection.key}
                title={activeSection.label}
              />
            ) : picked ? (
              <FieldForm
                ref={formRef}
                prefix={picked.fieldsPrefix}
                title={picked.item.label}
                onSaved={() => setPicked(null)}
              />
            ) : pickedType ? (
              <OnboardAliasListView
                sectionKey={pickedType.sectionKey}
                typeKey={pickedType.item.key}
                typeLabel={pickedType.item.label}
                onSelectAlias={(alias) => void openWithAlias(pickedType.item, pickedType.sectionKey, alias)}
                onBack={() => setPickedType(null)}
              />
            ) : (
              <SectionPicker
                sectionKey={activeSection.key}
                help={activeSection.help}
                onPick={(item) => void handlePick(item)}
                onSkip={() => void advanceSection()}
              />
            )}
          </div>
        )}
      </main>

    </div>
  );
}

function OnboardAliasListView({
  sectionKey,
  typeKey,
  typeLabel,
  onSelectAlias,
  onBack,
}: {
  sectionKey: string;
  typeKey: string;
  typeLabel: string;
  onSelectAlias: (alias: string) => void;
  onBack: () => void;
}) {
  const [aliases, setAliases] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [newAlias, setNewAlias] = useState('');

  const mapPath = sectionKey === 'providers'
    ? `providers.models.${typeKey}`
    : `channels.${typeKey}`;

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getMapKeys(mapPath)
      .then((r) => { if (!cancelled) setAliases(r.keys); })
      .catch(() => { if (!cancelled) setAliases([]); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [mapPath]);

  const submit = () => {
    const trimmed = newAlias.trim() || (aliases.length === 0 ? 'default' : `${aliases[0]}-2`);
    onSelectAlias(trimmed);
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
        {typeLabel} — select or create an alias
      </p>
      {loading ? (
        <div className="flex items-center justify-center py-12">
          <div className="h-8 w-8 border-2 rounded-full animate-spin"
            style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
        </div>
      ) : (
        <div className="surface-panel divide-y" style={{ borderColor: 'var(--pc-border)' }}>
          {aliases.map((alias) => (
            <button
              key={alias}
              type="button"
              onClick={() => onSelectAlias(alias)}
              className="w-full flex items-center justify-between gap-3 px-4 py-3 text-left text-sm transition-colors hover:opacity-90"
            >
              <div>
                <span style={{ color: 'var(--pc-text-primary)', fontWeight: 500 }}>{alias}</span>
                <code className="block text-xs mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>
                  {mapPath}.{alias}
                </code>
              </div>
              <ChevronRight className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-text-muted)' }} />
            </button>
          ))}
          <div className="flex items-center gap-2 px-4 py-3">
            <Plus className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-accent)' }} />
            <input
              type="text"
              className="input-electric flex-1 px-3 py-1.5 text-sm"
              placeholder={aliases.length === 0 ? 'default' : `${aliases[0]}-2`}
              value={newAlias}
              onChange={(e) => setNewAlias(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') submit(); }}
              // eslint-disable-next-line jsx-a11y/no-autofocus
              autoFocus={aliases.length === 0}
            />
            <button type="button" onClick={submit} className="btn-electric text-sm px-3 py-1.5 flex-shrink-0">
              Add
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function EditorLoading() {
  return (
    <div
      className="flex items-center justify-center rounded-xl border p-12"
      style={{
        borderColor: 'var(--pc-border)',
        background: 'var(--pc-bg-surface)',
      }}
    >
      <div
        className="h-6 w-6 border-2 rounded-full animate-spin"
        style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
      />
    </div>
  );
}

function isLastSection(sections: SectionInfo[], key: string): boolean {
  return sections[sections.length - 1]?.key === key;
}

function parseCompleted(v: unknown): string[] {
  if (Array.isArray(v)) return v.filter((x): x is string => typeof x === 'string');
  if (typeof v !== 'string' || !v.length || v === '<unset>') return [];
  try {
    const parsed = JSON.parse(v);
    if (Array.isArray(parsed)) {
      return parsed.filter((x): x is string => typeof x === 'string');
    }
  } catch {
    // CLI-display fallback: comma-separated.
  }
  return v.split(',').map((s) => s.trim()).filter(Boolean);
}
