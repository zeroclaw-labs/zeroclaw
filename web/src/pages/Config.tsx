// Schema-driven config editor (#6175). Same building blocks as /onboard
// but lands on a per-section overview: pick a section in the sidebar, see
// what's currently configured under it, click an item to edit, click +Add
// to instantiate a new entry.
//
// URL structure:
//   /config/:section             — section overview (configured items list)
//   /config/:section/:type       — alias list for a provider/channel type
//   /config/:section/:type/:alias — field form for a specific alias
//
// All section list / picker / field rendering comes from the shared
// SectionPicker + FieldForm components. NO hardcoded section names, field
// labels, dropdown options, or provider lists.

import { Suspense, lazy, useEffect, useMemo, useState } from 'react';
import { Link, useLocation, useNavigate, useParams } from 'react-router-dom';
import { ArrowLeft, ChevronRight, Plus, Sparkles } from 'lucide-react';
import {
  ApiError,
  getDrift,
  getMapKeys,
  getSections,
  selectSectionItem,
  type DriftEntry,
  type PickerItem,
  type SectionInfo,
} from '../lib/api';
import FieldForm from '../components/onboard/FieldForm';
import ReloadDaemonButton from '../components/onboard/ReloadDaemonButton';
import SectionPicker from '../components/onboard/SectionPicker';

// Personality pulls in CodeMirror + markdown rendering (~270KB gzipped).
// Lazy-load so the cost isn't paid until the user opens that section.
const PersonalityEditor = lazy(
  () => import('../components/onboard/PersonalityEditor'),
);

// Synthetic sections that aren't backed by a config-schema prefix. They
// render a dedicated component instead of the generic FieldForm/picker
// flow but otherwise slot into the same group/sidebar/breadcrumb plumbing.
const SYNTHETIC_SECTIONS: SectionInfo[] = [
  {
    key: 'personality',
    label: 'Personality',
    help: 'Edit the markdown files that shape your agent — SOUL, IDENTITY, USER, etc.',
    has_picker: false,
    completed: false,
    group: 'Foundation',
  },
];

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
  // URL params drive the view. No internal mode state for picker/form —
  // the address bar is the source of truth.
  //   :section              → section overview
  //   :section/:type        → alias list (providers/channels) or picker (others)
  //   :section/:type/:alias → field form
  const {
    section: sectionParam,
    type: typeParam,
    alias: aliasParam,
  } = useParams<{ section?: string; type?: string; alias?: string }>();
  const location = useLocation();
  const navigate = useNavigate();
  const lockedSection = location.pathname.startsWith('/setup/') ? sectionParam : undefined;
  const [sections, setSections] = useState<SectionInfo[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [drifted, setDrifted] = useState<DriftEntry[]>([]);
  const fetchDrift = () => {
    void getDrift()
      .then((r) => setDrifted(r.drifted ?? []))
      .catch(() => undefined);
  };
  useEffect(fetchDrift, [activeKey]);

  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getSections()
      .then((resp) => {
        if (cancelled) return;
        const merged = [
          ...resp.sections,
          ...SYNTHETIC_SECTIONS.filter(
            (synth) => !resp.sections.some((s) => s.key === synth.key),
          ),
        ];
        setSections(merged);
        const initialKey = sectionParam && merged.find((s) => s.key === sectionParam)
          ? sectionParam
          : merged[0]?.key ?? null;
        setActiveKey(initialKey);
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
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!sectionParam || sections.length === 0) return;
    if (sections.some((s) => s.key === sectionParam) && sectionParam !== activeKey) {
      setActiveKey(sectionParam);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sectionParam, sections]);

  const activeSection = useMemo(
    () => sections.find((s) => s.key === activeKey) ?? null,
    [sections, activeKey],
  );

  const goToSection = (key: string) => {
    setActiveKey(key);
    if (!lockedSection) {
      navigate(`/config/${encodeURIComponent(key)}`);
    }
  };

  // Navigate to alias list for a provider/channel type.
  const goToType = (sectionKey: string, typeKey: string) => {
    navigate(`/config/${encodeURIComponent(sectionKey)}/${encodeURIComponent(typeKey)}`);
  };

  // Navigate to the form for a specific alias. Calls selectSectionItem
  // to instantiate the entry if needed, then navigates to the alias URL.
  const goToAlias = async (sectionKey: string, typeKey: string, alias: string) => {
    try {
      await selectSectionItem(sectionKey, typeKey, alias);
      navigate(
        `/config/${encodeURIComponent(sectionKey)}/${encodeURIComponent(typeKey)}/${encodeURIComponent(alias)}`,
      );
    } catch (e) {
      if (e instanceof ApiError) {
        setError(`[${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(e instanceof Error ? e.message : String(e));
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

  // Determine what to render in the main pane based on URL params.
  const needsAliasTier =
    activeSection?.has_picker &&
    (activeSection.key === 'providers' || activeSection.key === 'channels');

  const mainContent = (() => {
    if (!activeSection) return null;

    if (activeSection.key === 'personality') {
      return (
        <Suspense
          fallback={
            <div
              className="flex items-center justify-center rounded-xl border p-12"
              style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
            >
              <div
                className="h-6 w-6 border-2 rounded-full animate-spin"
                style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
              />
            </div>
          }
        >
          <PersonalityEditor />
        </Suspense>
      );
    }

    if (!activeSection.has_picker) {
      return (
        <FieldForm
          key={reloadKey}
          prefix={activeSection.key}
          title={activeSection.label}
          onSaved={fetchDrift}
          drift={drifted}
        />
      );
    }

    // /config/:section/:type/:alias — field form
    if (typeParam && aliasParam) {
      const fieldsPrefix = needsAliasTier
        ? activeSection.key === 'providers'
          ? `providers.models.${typeParam}.${aliasParam}`
          : `channels.${typeParam}.${aliasParam}`
        : typeParam;
      return (
        <div className="flex flex-col gap-3">
          <button
            type="button"
            onClick={() => navigate(-1)}
            className="btn-secondary inline-flex items-center gap-2 text-sm px-3 py-1.5 self-start"
          >
            <ArrowLeft className="h-4 w-4" />
            Back
          </button>
          <FieldForm
            key={`${reloadKey}-${fieldsPrefix}`}
            prefix={fieldsPrefix}
            title={`${typeParam} / ${aliasParam}`}
            onSaved={fetchDrift}
            drift={drifted}
          />
        </div>
      );
    }

    // /config/:section/:type — alias list (providers/channels) or direct form
    if (typeParam && needsAliasTier) {
      return (
        <AliasListView
          sectionKey={activeSection.key}
          typeKey={typeParam}
          onSelectAlias={async (alias) => {
            await selectSectionItem(activeSection.key, typeParam, alias);
            navigate(
              `/config/${encodeURIComponent(activeSection.key)}/${encodeURIComponent(typeParam)}/${encodeURIComponent(alias)}`,
            );
          }}
          onBack={() => navigate(`/config/${encodeURIComponent(activeSection.key)}`)}
        />
      );
    }

    // /config/:section — section overview (configured items) + picker
    if (typeParam) {
      // Non-alias-tiered section with a type in the URL: treat as form
      return (
        <div className="flex flex-col gap-3">
          <button
            type="button"
            onClick={() => navigate(`/config/${encodeURIComponent(activeSection.key)}`)}
            className="btn-secondary inline-flex items-center gap-2 text-sm px-3 py-1.5 self-start"
          >
            <ArrowLeft className="h-4 w-4" />
            Back to {activeSection.label}
          </button>
          <FieldForm
            key={`${reloadKey}-${typeParam}`}
            prefix={typeParam}
            title={typeParam}
            onSaved={fetchDrift}
            drift={drifted}
          />
        </div>
      );
    }

    // /config/:section — overview + picker
    return (
      <SectionOverview
        section={activeSection}
        onPickType={(typeKey) => {
          if (needsAliasTier) {
            goToType(activeSection.key, typeKey);
          } else {
            void (async () => {
              try {
                const resp = await selectSectionItem(activeSection.key, typeKey);
                navigate(
                  `/config/${encodeURIComponent(activeSection.key)}/${encodeURIComponent(typeKey)}`,
                  { state: { fieldsPrefix: resp.fields_prefix } },
                );
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              }
            })();
          }
        }}
        onPickAlias={(typeKey, alias) => void goToAlias(activeSection.key, typeKey, alias)}
        sectionUrl={`/config/${encodeURIComponent(activeSection.key)}`}
        reloadKey={reloadKey}
        fetchDrift={fetchDrift}
        drifted={drifted}
      />
    );
  })();

  // Breadcrumb segments
  const crumbs: Array<{ label: string; url?: string }> = [
    { label: 'Config', url: '/config' },
    {
      label: activeSection?.label ?? '',
      url: activeSection
        ? `/config/${encodeURIComponent(activeSection.key)}`
        : undefined,
    },
  ];
  if (typeParam) crumbs.push({ label: typeParam, url: typeParam && aliasParam ? `/config/${encodeURIComponent(sectionParam ?? '')}/${encodeURIComponent(typeParam)}` : undefined });
  if (aliasParam) crumbs.push({ label: aliasParam });

  return (
    <div className="flex h-full overflow-hidden">
      {!lockedSection && (
        <aside
          className="w-56 flex-shrink-0 border-r overflow-y-auto"
          style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
        >
          <nav className="flex flex-col">
            {GROUP_ORDER.map((groupName) => {
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
                        background: s.key === activeKey ? 'var(--pc-accent-glow)' : 'transparent',
                        color: s.key === activeKey ? 'var(--pc-accent)' : 'var(--pc-text-primary)',
                        fontWeight: s.key === activeKey ? 600 : 400,
                        borderLeft: s.key === activeKey
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

      <main className="flex-1 overflow-y-auto p-6">
        {activeSection && (
          <div className="flex flex-col gap-4 max-w-3xl">
            {/* Breadcrumb */}
            <div className="flex items-center justify-between gap-3 flex-wrap">
              <div
                className="text-sm flex items-center gap-1.5 flex-wrap"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                {crumbs.map((crumb, i) => (
                  <span key={i} className="flex items-center gap-1.5">
                    {i > 0 && <ChevronRight className="h-3 w-3" />}
                    {crumb.url && i < crumbs.length - 1 ? (
                      <span
                        style={{ color: 'var(--pc-text-secondary)', cursor: 'pointer' }}
                        onClick={() => navigate(crumb.url!)}
                      >
                        {crumb.label}
                      </span>
                    ) : (
                      <span style={{ color: 'var(--pc-accent)', fontWeight: 600 }}>
                        {crumb.label}
                      </span>
                    )}
                  </span>
                ))}
              </div>
              <div className="flex items-center gap-2">
                <Link
                  to="/onboard"
                  className="btn-secondary inline-flex items-center gap-1.5 text-xs px-3 py-1.5"
                  title="Walk the first-run onboarding wizard again"
                >
                  <Sparkles className="h-3.5 w-3.5" />
                  Run onboarding again
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

            {mainContent}
          </div>
        )}
      </main>
    </div>
  );
}

// Alias list page: /config/:section/:type
// Shows existing aliases as clickable rows + an inline "new alias" input.
function AliasListView({
  sectionKey,
  typeKey,
  onSelectAlias,
  onBack,
}: {
  sectionKey: string;
  typeKey: string;
  onSelectAlias: (alias: string) => Promise<void>;
  onBack: () => void;
}) {
  const [aliases, setAliases] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [newAlias, setNewAlias] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [aliasError, setAliasError] = useState<string | null>(null);

  const mapPath = sectionKey === 'providers'
    ? `providers.models.${typeKey}`
    : `channels.${typeKey}`;

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getMapKeys(mapPath)
      .then((r) => { if (!cancelled) setAliases(r.keys); })
      .catch((e) => {
        if (!cancelled) {
          setAliases([]);
          setError(e instanceof Error ? e.message : String(e));
        }
      })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [mapPath]);

  const submit = async () => {
    const trimmed = newAlias.trim() || (aliases.length === 0 ? 'default' : `${aliases[0]}-2`);
    setAliasError(null);
    try {
      await onSelectAlias(trimmed);
    } catch (e) {
      setAliasError(
        e instanceof ApiError ? e.envelope.message : (e instanceof Error ? e.message : String(e)),
      );
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <button
        type="button"
        onClick={onBack}
        className="btn-secondary inline-flex items-center gap-2 text-sm px-3 py-1.5 self-start"
      >
        <ArrowLeft className="h-4 w-4" />
        Back
      </button>

      {error && (
        <div
          className="rounded-xl border p-3 text-sm"
          style={{ background: 'rgba(239,68,68,0.08)', borderColor: 'rgba(239,68,68,0.2)', color: '#f87171' }}
        >
          {error}
        </div>
      )}

      {loading ? (
        <div className="flex items-center justify-center py-12">
          <div
            className="h-8 w-8 border-2 rounded-full animate-spin"
            style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
          />
        </div>
      ) : (
        <div
          className="surface-panel divide-y"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          {aliases.map((alias) => (
            <button
              key={alias}
              type="button"
              onClick={() => {
                onSelectAlias(alias).catch((e) => {
                  setError(
                    e instanceof ApiError
                      ? `[${e.envelope.code}] ${e.envelope.message}`
                      : (e instanceof Error ? e.message : String(e)),
                  );
                });
              }}
              className="w-full flex items-center justify-between gap-3 px-4 py-3 text-left text-sm transition-colors hover:opacity-90"
            >
              <div>
                <span style={{ color: 'var(--pc-text-primary)', fontWeight: 500 }}>{alias}</span>
                <code
                  className="block text-xs mt-0.5"
                  style={{ color: 'var(--pc-text-faint)' }}
                >
                  {mapPath}.{alias}
                </code>
              </div>
              <ChevronRight className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-text-muted)' }} />
            </button>
          ))}

          {/* Inline new alias row */}
          <div className="flex flex-col gap-1 px-4 py-3">
            <div className="flex items-center gap-2">
              <input
                type="text"
                className="input-electric flex-1 px-3 py-1.5 text-sm"
                placeholder={aliases.length === 0 ? 'default' : `${aliases[0]}-2`}
                value={newAlias}
                onChange={(e) => { setNewAlias(e.target.value); setAliasError(null); }}
                onKeyDown={(e) => { if (e.key === 'Enter') void submit(); }}
              />
              <button
                type="button"
                onClick={() => void submit()}
                className="btn-electric text-sm px-3 py-1.5 flex-shrink-0"
              >
                Add
              </button>
            </div>
            {aliasError && (
              <p className="text-xs" style={{ color: 'var(--color-status-error)' }}>{aliasError}</p>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

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
  onPickType: (typeKey: string) => void;
  onPickAlias: (typeKey: string, alias: string) => void;
  sectionUrl: string;
  reloadKey: number;
  fetchDrift: () => void;
  drifted: DriftEntry[];
}

function SectionOverview({
  section,
  onPickType,
  onPickAlias,
  sectionUrl,
}: SectionOverviewProps) {
  const [showPicker, setShowPicker] = useState(false);

  if (showPicker) {
    return (
      <div className="flex flex-col gap-3">
        <button
          type="button"
          onClick={() => setShowPicker(false)}
          className="btn-secondary inline-flex items-center gap-2 text-sm px-3 py-1.5 self-start"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to {section.label}
        </button>
        <SectionPicker
          sectionKey={section.key}
          help={section.help}
          onPick={(item) => { setShowPicker(false); onPickType(item.key); }}
          onSkip={() => setShowPicker(false)}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
          {section.help}
        </p>
        <button
          type="button"
          onClick={() => setShowPicker(true)}
          className="btn-electric flex items-center gap-2 text-sm px-3 py-2 flex-shrink-0"
        >
          <Plus className="h-4 w-4" />
          Add
        </button>
      </div>
      <ConfiguredOnlyPicker
        section={section}
        onPickType={onPickType}
        onPickAlias={onPickAlias}
        sectionUrl={sectionUrl}
      />
    </div>
  );
}

interface ConfiguredOnlyPickerProps {
  section: SectionInfo;
  onPickType: (typeKey: string) => void;
  onPickAlias: (typeKey: string, alias: string) => void;
  sectionUrl: string;
}

function ConfiguredOnlyPicker({ section, onPickType }: ConfiguredOnlyPickerProps) {
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
    return () => { cancelled = true; };
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
          onClick={() => onPickType(item.key)}
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
