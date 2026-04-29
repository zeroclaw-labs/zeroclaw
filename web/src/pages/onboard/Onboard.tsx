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

import { useEffect, useMemo, useState } from 'react';
import { Check, ChevronRight } from 'lucide-react';
import {
  ApiError,
  getProp,
  getSections,
  patchConfig,
  selectSectionItem,
  type PickerItem,
  type SectionInfo,
} from '../../lib/api';
import FieldForm from '../../components/onboard/FieldForm';
import SectionPicker from '../../components/onboard/SectionPicker';

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
] as const;

export default function Onboard() {
  const [sections, setSections] = useState<SectionInfo[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [picked, setPicked] = useState<{ item: PickerItem; fieldsPrefix: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

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
          const s = byKey.get(k);
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
  };

  const handlePick = async (item: PickerItem) => {
    if (!activeSection) return;
    try {
      const resp = await selectSectionItem(activeSection.key, item.key);
      setPicked({ item, fieldsPrefix: resp.fields_prefix });
    } catch (e) {
      if (e instanceof ApiError) {
        setError(`Couldn't select ${item.label}: [${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(`Couldn't select ${item.label}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  };

  const advanceSection = async () => {
    if (!activeSection) return;
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
    } else {
      // Wizard done — stay on current section but clear picked state.
      setPicked(null);
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
                    color: picked
                      ? 'var(--pc-text-secondary)'
                      : 'var(--pc-accent)',
                    cursor: picked ? 'pointer' : 'default',
                    fontWeight: picked ? 400 : 600,
                  }}
                  onClick={() => picked && setPicked(null)}
                >
                  {activeSection.label}
                </span>
                {picked && (
                  <>
                    <ChevronRight className="h-3 w-3" />
                    <span
                      style={{ color: 'var(--pc-accent)', fontWeight: 600 }}
                    >
                      {picked.item.label}
                    </span>
                  </>
                )}
              </div>
              <button
                type="button"
                onClick={() => void advanceSection()}
                className="btn-electric inline-flex items-center gap-1.5 text-sm px-4 py-2"
              >
                {isLastSection(sections, activeSection.key) ? 'Done' : 'Next ▶'}
              </button>
            </div>

            {/* Picker view OR form view */}
            {!activeSection.has_picker ? (
              // Direct-form sections (Workspace, Hardware) skip the picker.
              <FieldForm
                prefix={activeSection.key}
                title={activeSection.label}
                onSaved={advanceSection}
              />
            ) : !picked ? (
              <SectionPicker
                sectionKey={activeSection.key}
                help={activeSection.help}
                onPick={(item) => void handlePick(item)}
                doneLabel={
                  isLastSection(sections, activeSection.key)
                    ? 'Done'
                    : 'Next ▶'
                }
                onDone={advanceSection}
              />
            ) : (
              <FieldForm
                prefix={picked.fieldsPrefix}
                title={picked.item.label}
                onSaved={() => {
                  // Return to the picker so the user can add another (or hit Done).
                  setPicked(null);
                }}
              />
            )}
          </div>
        )}
      </main>
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
