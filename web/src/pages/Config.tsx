// Schema-driven config editor (#6175). Same building blocks as /onboard
// but lands on a per-section overview: pick a section in the sidebar, see
// what's currently configured under it, click an item to edit, click +Add
// to instantiate a new entry.
//
// All section list / picker / field rendering comes from the shared
// SectionPicker + FieldForm components. NO hardcoded section names, field
// labels, dropdown options, or provider lists.

import { useEffect, useMemo, useState } from 'react';
import { Link, useParams } from 'react-router-dom';
import { ArrowLeft, ChevronRight, Plus, Sparkles } from 'lucide-react';
import {
  ApiError,
  getDrift,
  getSections,
  selectSectionItem,
  type ConfigApiError,
  type DriftEntry,
  type PickerItem,
  type SectionInfo,
} from '../lib/api';
import FieldForm from '../components/onboard/FieldForm';
import ReloadDaemonButton from '../components/onboard/ReloadDaemonButton';
import SectionPicker from '../components/onboard/SectionPicker';

type Mode =
  | { kind: 'section-overview' }
  // 'picker' shows the catalog so the user can pick a new item to add or
  // an existing one to edit. We reuse the same picker for both: items
  // already-configured carry the badge so the user knows what's there.
  | { kind: 'picker' }
  | { kind: 'form'; item: PickerItem; fieldsPrefix: string };

// Display order for the curated sidebar groups. Each `SectionInfo.group`
// from the gateway lands in one of these buckets (anything else falls
// into "Other"). Schema-attribute-driven grouping replaces this in v3 /
// #5947.
//
// Foundation leads — Workspace / Providers / Channels / Memory /
// Hardware / Tunnel are the most-edited sections, surfaced first inside
// the Config explorer instead of as duplicate top-level nav entries.
// The setup wizard at /onboard walks the same six (reachable via the
// "Run setup again" link in the breadcrumb row).
const GROUP_ORDER = [
  'Foundation',
  'Agent',
  'Multi-agent',
  'Tools',
  'Integrations',
  'Network',
  'Storage',
  'Operations',
  'Other',
] as const;

export default function Config() {
  // `:section` route param locks the page to that single section (used by
  // the promoted top-level routes like `/setup/providers`); without it we
  // render the full multi-section explorer.
  const { section: lockedSection } = useParams<{ section?: string }>();
  const [sections, setSections] = useState<SectionInfo[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>({ kind: 'section-overview' });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Single page-level drift state. Refreshed on every section change,
  // after a daemon reload (ReloadDaemonButton.onReloaded), and after
  // any successful save in a rendered FieldForm (onSaved). One source,
  // four refresh points. FieldForm is drift-agnostic — no in-form
  // banner or per-field indicator.
  const [drifted, setDrifted] = useState<DriftEntry[]>([]);
  const fetchDrift = () => {
    void getDrift()
      .then((r) => setDrifted(r.drifted ?? []))
      .catch(() => undefined);
  };
  useEffect(fetchDrift, [activeKey]);

  // Bumped after a successful daemon reload — used as `key` on
  // <FieldForm> so React fully remounts and the new instance refetches
  // values from the freshly-loaded gateway. Without this, an unchanged
  // prefix prop leaves FieldForm's `useEffect([prefix])` dormant and
  // the form keeps displaying values from before the reload.
  const [reloadKey, setReloadKey] = useState(0);


  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getSections()
      .then((resp) => {
        if (cancelled) return;
        setSections(resp.sections);
        // Locked-section view: pick the requested section. Falls back to
        // first available if the URL specifies an unknown key.
        const initialKey = lockedSection
          && resp.sections.find((s) => s.key === lockedSection)
          ? lockedSection
          : resp.sections[0]?.key ?? null;
        setActiveKey(initialKey);
        setMode({ kind: 'section-overview' });
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
  }, [lockedSection]);

  const activeSection = useMemo(
    () => sections.find((s) => s.key === activeKey) ?? null,
    [sections, activeKey],
  );

  const goToSection = (key: string) => {
    setActiveKey(key);
    setMode({ kind: 'section-overview' });
  };

  const handlePick = async (item: PickerItem) => {
    if (!activeSection) return;
    try {
      const resp = await selectSectionItem(activeSection.key, item.key);
      setMode({ kind: 'form', item, fieldsPrefix: resp.fields_prefix });
    } catch (e) {
      if (e instanceof ApiError) {
        setError(`Couldn't open ${item.label}: [${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(`Couldn't open ${item.label}: ${e instanceof Error ? e.message : String(e)}`);
      }
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
      {/* Sidebar — hidden for the locked single-section view (the
          top-level /setup/<section> routes). The main app sidebar
          handles section selection in that case. */}
      {!lockedSection && (
      <aside
        className="w-56 flex-shrink-0 border-r overflow-y-auto"
        style={{
          borderColor: 'var(--pc-border)',
          background: 'var(--pc-bg-surface)',
        }}
      >
        <nav className="flex flex-col">
          {GROUP_ORDER.map((groupName) => {
            // Sections whose `group` isn't in GROUP_ORDER bucket into
            // "Other" so a backend rename never silently drops them
            // (e.g. "Onboarding" → "Foundation" before the daemon is
            // restarted on the new binary).
            const known = new Set(GROUP_ORDER);
            const items = sections
              .filter((s) =>
                groupName === 'Other'
                  ? s.group === 'Other' || !known.has(s.group as typeof GROUP_ORDER[number])
                  : s.group === groupName,
              )
              .sort((a, b) => a.label.localeCompare(b.label));
            if (items.length === 0) return null;
            return (
              <div key={groupName}>
                <div
                  className="px-4 pt-4 pb-1.5 text-xs font-semibold uppercase tracking-wider"
                  style={{ color: 'var(--pc-text-secondary)' }}
                >
                  {groupName}
                </div>
                {items.map((s) => (
                  <button
                    key={s.key}
                    type="button"
                    onClick={() => goToSection(s.key)}
                    className="flex items-center justify-between gap-2 px-4 py-2 text-sm text-left transition-colors"
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
                    <span>{s.label}</span>
                    {s.key === activeKey && <ChevronRight className="h-3.5 w-3.5" />}
                  </button>
                ))}
              </div>
            );
          })}
        </nav>
      </aside>
      )}

      {/* Main pane */}
      <main className="flex-1 overflow-y-auto p-6">
        {activeSection && (
          <div className="flex flex-col gap-4 max-w-3xl">
            {/* Breadcrumb + Reload daemon button on the right */}
            <div className="flex items-center justify-between gap-3 flex-wrap">
              <div
                className="text-sm flex items-center gap-1.5 flex-wrap"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                <span style={{ color: 'var(--pc-text-secondary)' }}>Config</span>
                <ChevronRight className="h-3 w-3" />
                <span
                  style={{
                    color: mode.kind === 'section-overview'
                      ? 'var(--pc-accent)'
                      : 'var(--pc-text-secondary)',
                    cursor: mode.kind !== 'section-overview' ? 'pointer' : 'default',
                    fontWeight: mode.kind === 'section-overview' ? 600 : 400,
                  }}
                  onClick={() => setMode({ kind: 'section-overview' })}
                >
                  {activeSection.label}
                </span>
                {mode.kind === 'form' && (
                  <>
                    <ChevronRight className="h-3 w-3" />
                    <span style={{ color: 'var(--pc-accent)', fontWeight: 600 }}>
                      {mode.item.label}
                    </span>
                  </>
                )}
              </div>
              <div className="flex items-center gap-2">
                {/* Setup wizard isn't in the global navbar (it's a one-shot
                    first-run flow), so contributors / re-installers reach it
                    from here. Mirrors the in-progress signal Audacity88 and
                    iLTeoooD raised on PR #6179. */}
                <Link
                  to="/onboard"
                  className="btn-secondary inline-flex items-center gap-1.5 text-xs px-3 py-1.5"
                  title="Walk the first-run setup wizard again"
                >
                  <Sparkles className="h-3.5 w-3.5" />
                  Run setup again
                </Link>
                <ReloadDaemonButton
                  onReloaded={() => {
                    goToSection(activeSection.key);
                    fetchDrift();
                    setReloadKey((n) => n + 1);
                  }}
                />
              </div>
            </div>

            {drifted.length > 0 && (
              <PageDriftBanner
                drifted={drifted}
                onReloaded={() => {
                  goToSection(activeSection.key);
                  fetchDrift();
                  setReloadKey((n) => n + 1);
                }}
              />
            )}

            {/* Section overview / picker / form */}
            {!activeSection.has_picker ? (
              // Direct-form sections (Workspace, Hardware): no picker, just
              // show the form rooted at the section's path prefix.
              <FieldForm
                key={reloadKey}
                prefix={activeSection.key}
                title={activeSection.label}
                onSaved={fetchDrift}
              />
            ) : mode.kind === 'section-overview' ? (
              <SectionOverview
                section={activeSection}
                onAdd={() => setMode({ kind: 'picker' })}
                onEdit={(item, prefix) =>
                  setMode({ kind: 'form', item, fieldsPrefix: prefix })
                }
              />
            ) : mode.kind === 'picker' ? (
              <div className="flex flex-col gap-3">
                <button
                  type="button"
                  onClick={() => setMode({ kind: 'section-overview' })}
                  className="btn-secondary inline-flex items-center gap-2 text-sm px-3 py-1.5 self-start"
                >
                  <ArrowLeft className="h-4 w-4" />
                  Back to {activeSection.label}
                </button>
                <SectionPicker
                  sectionKey={activeSection.key}
                  help={activeSection.help}
                  onPick={(item) => void handlePick(item)}
                  onSkip={() => setMode({ kind: 'section-overview' })}
                />
              </div>
            ) : (
              <div className="flex flex-col gap-3">
                <button
                  type="button"
                  onClick={() => setMode({ kind: 'section-overview' })}
                  className="btn-secondary inline-flex items-center gap-2 text-sm px-3 py-1.5 self-start"
                >
                  <ArrowLeft className="h-4 w-4" />
                  Back to {activeSection.label}
                </button>
                <FieldForm
                  key={reloadKey}
                  prefix={mode.fieldsPrefix}
                  title={mode.item.label}
                  onSaved={fetchDrift}
                />
              </div>
            )}
          </div>
        )}
      </main>
    </div>
  );
}

// Single page-level drift banner. Embeds `<ReloadDaemonButton>`
// directly so the inline reload action is the same component the
// top-right toolbar uses — same modal, same /health poll, same
// onReloaded callback, no parallel reload code.
function PageDriftBanner({
  drifted,
  onReloaded,
}: {
  drifted: DriftEntry[];
  onReloaded: () => void;
}) {
  return (
    <div
      className="rounded-xl border p-3 text-sm flex flex-col gap-2"
      style={{
        borderColor: 'var(--color-status-warning, #f5b400)',
        background: 'rgba(245, 180, 0, 0.06)',
      }}
    >
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <span style={{ color: 'var(--pc-text-primary)' }}>
          ⚠ {drifted.length} path{drifted.length === 1 ? '' : 's'} differ
          {drifted.length === 1 ? 's' : ''} from on-disk
        </span>
        <ReloadDaemonButton onReloaded={onReloaded} />
      </div>
      <ul
        className="text-xs flex flex-col gap-0.5"
        style={{ color: 'var(--pc-text-muted)' }}
      >
        {drifted.slice(0, 6).map((d) => (
          <li key={d.path} className="font-mono break-all">
            {d.path}
            {d.secret && (
              <span style={{ color: 'var(--pc-text-faint)' }}>
                {' '}
                (secret — values not shown)
              </span>
            )}
          </li>
        ))}
        {drifted.length > 6 && (
          <li style={{ color: 'var(--pc-text-faint)' }}>
            …and {drifted.length - 6} more
          </li>
        )}
      </ul>
    </div>
  );
}

interface SectionOverviewProps {
  section: SectionInfo;
  onAdd: () => void;
  onEdit: (item: PickerItem, fieldsPrefix: string) => void;
}

function SectionOverview({ section, onAdd, onEdit }: SectionOverviewProps) {
  // The overview is just the section picker filtered to configured items.
  // Reuse SectionPicker by treating its "Done" button as "+ Add new". For
  // simplicity, embed the picker directly with the picker semantics tuned
  // for editing — clicking a row opens the form for it.
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <p
          className="text-sm"
          style={{ color: 'var(--pc-text-secondary)' }}
        >
          {section.help}
        </p>
        <button
          type="button"
          onClick={onAdd}
          className="btn-electric flex items-center gap-2 text-sm px-3 py-2 flex-shrink-0"
        >
          <Plus className="h-4 w-4" />
          Add
        </button>
      </div>
      {/* The picker handles fetching, filtering, click. Treat onPick as
          "edit this item" — selectSectionItem returns the existing fields
          prefix idempotently when the entry already exists. */}
      <ConfiguredOnlyPicker section={section} onEdit={onEdit} />
    </div>
  );
}

interface ConfiguredOnlyPickerProps {
  section: SectionInfo;
  onEdit: (item: PickerItem, fieldsPrefix: string) => void;
}

/**
 * Strips the picker down to items that are already configured (badge =
 * "configured" or "active"). Empty state guides the user to + Add.
 */
function ConfiguredOnlyPicker({ section, onEdit }: ConfiguredOnlyPickerProps) {
  const [items, setItems] = useState<PickerItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    import('../lib/api').then(({ getSectionPicker }) =>
      getSectionPicker(section.key)
        .then((resp) => {
          if (cancelled) return;
          setItems(
            resp.items.filter(
              (i) => i.badge === 'configured' || i.badge === 'active',
            ),
          );
        })
        .catch((e) => {
          if (cancelled) return;
          if (e instanceof ApiError) {
            setError(`[${e.envelope.code}] ${e.envelope.message}`);
          } else {
            setError(`Couldn't load configured items: ${e instanceof Error ? e.message : String(e)}`);
          }
        })
        .finally(() => !cancelled && setLoading(false)),
    );
    return () => {
      cancelled = true;
    };
  }, [section.key]);

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

  if (error) {
    return (
      <div
        className="rounded-xl border p-3 text-sm"
        style={{
          background: 'rgba(239, 68, 68, 0.08)',
          borderColor: 'rgba(239, 68, 68, 0.2)',
          color: '#f87171',
        }}
      >
        {error}
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div
        className="surface-panel p-8 text-center text-sm"
        style={{ color: 'var(--pc-text-muted)' }}
      >
        Nothing configured under <strong>{section.label}</strong> yet. Click{' '}
        <strong>+ Add</strong> to get started.
      </div>
    );
  }

  return (
    <div
      className="surface-panel divide-y"
      style={{ borderColor: 'var(--pc-border)' }}
    >
      {items.map((item) => (
        <button
          key={item.key}
          type="button"
          onClick={async () => {
            try {
              const resp = await (
                await import('../lib/api')
              ).selectSectionItem(section.key, item.key);
              onEdit(item, resp.fields_prefix);
            } catch (e) {
              const msg =
                e instanceof ApiError
                  ? `[${(e.envelope as ConfigApiError).code}] ${e.envelope.message}`
                  : e instanceof Error
                  ? e.message
                  : String(e);
              alert(`Couldn't open ${item.label}: ${msg}`);
            }
          }}
          className="w-full flex items-center justify-between gap-3 px-4 py-3 text-left transition-colors hover:opacity-90"
        >
          <div className="flex-1 min-w-0">
            <div
              className="text-sm font-medium"
              style={{ color: 'var(--pc-text-primary)' }}
            >
              {item.label}
            </div>
            <code
              className="block text-xs mt-0.5"
              style={{ color: 'var(--pc-text-faint)' }}
            >
              {item.key}
            </code>
          </div>
          <ChevronRight
            className="h-4 w-4 flex-shrink-0"
            style={{ color: 'var(--pc-text-muted)' }}
          />
        </button>
      ))}
    </div>
  );
}
