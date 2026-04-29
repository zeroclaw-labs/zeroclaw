// Schema-driven config editor (#6175). Same building blocks as /onboard
// but lands on a per-section overview: pick a section in the sidebar, see
// what's currently configured under it, click an item to edit, click +Add
// to instantiate a new entry.
//
// All section list / picker / field rendering comes from the shared
// SectionPicker + FieldForm components. NO hardcoded section names, field
// labels, dropdown options, or provider lists.

import { useEffect, useMemo, useState } from 'react';
import { ArrowLeft, ChevronRight, Plus } from 'lucide-react';
import {
  ApiError,
  getSections,
  selectSectionItem,
  type ConfigApiError,
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

export default function Config() {
  const [sections, setSections] = useState<SectionInfo[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>({ kind: 'section-overview' });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getSections()
      .then((resp) => {
        if (cancelled) return;
        setSections(resp.sections);
        setActiveKey(resp.sections[0]?.key ?? null);
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
              <span>{s.label}</span>
              {s.key === activeKey && <ChevronRight className="h-3.5 w-3.5" />}
            </button>
          ))}
        </nav>
      </aside>

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
              <ReloadDaemonButton onReloaded={() => goToSection(activeSection.key)} />
            </div>

            {/* Section overview / picker / form */}
            {!activeSection.has_picker ? (
              // Direct-form sections (Workspace, Hardware): no picker, just
              // show the form rooted at the section's path prefix.
              <FieldForm prefix={activeSection.key} title={activeSection.label} />
            ) : mode.kind === 'section-overview' ? (
              <SectionOverview
                section={activeSection}
                onAdd={() => setMode({ kind: 'picker' })}
                onEdit={(item, prefix) =>
                  setMode({ kind: 'form', item, fieldsPrefix: prefix })
                }
              />
            ) : mode.kind === 'picker' ? (
              <SectionPicker
                sectionKey={activeSection.key}
                help={activeSection.help}
                onPick={(item) => void handlePick(item)}
                doneLabel="Back to overview"
                onDone={() => setMode({ kind: 'section-overview' })}
              />
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
                  prefix={mode.fieldsPrefix}
                  title={mode.item.label}
                />
              </div>
            )}
          </div>
        )}
      </main>
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
