import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useLocation, useNavigate, useParams } from 'react-router-dom';
import { PanelLeft, Plus, Search, Workflow, X } from 'lucide-react';
import { listRuns, listSops, type SopRunSummary, type SopSummary } from '@/lib/sops';
import { useWorkspaceVisible, VisibleWorkspace } from '@/components/layout/WorkspaceOutlet';
import { SopEditor } from '@/pages/Sops';
import { t } from '@/lib/i18n';

export default function SopWorkspace() {
  const { name } = useParams();
  const location = useLocation();
  const visible = useWorkspaceVisible();
  const locationRef = useRef(location);
  if (visible) locationRef.current = location;
  const { pathname, search } = locationRef.current;
  const navigate = useNavigate();
  const [sops, setSops] = useState<SopSummary[]>([]);
  const [runs, setRuns] = useState<SopRunSummary[]>([]);
  const [query, setQuery] = useState('');
  const [error, setError] = useState('');
  const [runError, setRunError] = useState('');
  const [loaded, setLoaded] = useState(false);
  const [revision, setRevision] = useState(0);
  const [libraryOpen, setLibraryOpen] = useState(
    () => window.matchMedia('(min-width: 1280px)').matches,
  );
  const editors = useRef<Record<string, { id: string; element: ReactNode }>>({});
  const lastSelection = useRef<string | null | undefined>(undefined);
  const selectionLocation = useRef<string | null>(null);
  // A saved new draft acquires its name before Router commits navigation.
  // Only a new navigation may replace that selection, not an intervening poll.
  if (visible && selectionLocation.current !== location.key) {
    if (pathname === '/sops/new') lastSelection.current = null;
    else if (name) lastSelection.current = name;
    selectionLocation.current = location.key;
  }
  const selected = lastSelection.current !== undefined ? lastSelection.current : sops[0]?.name;
  const selectedKey = selected ?? '@new';
  const runId = new URLSearchParams(search).get('run');

  useEffect(() => {
    let cancelled = false;
    void listSops()
      .then((items) => {
        if (!cancelled) {
          setSops(items);
          setError('');
        }
      })
      .catch((e: Error) => {
        if (!cancelled) setError(e.message);
      })
      .finally(() => {
        if (!cancelled) setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [revision]);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const items = await listRuns();
        if (!cancelled) {
          setRuns(items);
          setRunError('');
        }
      } catch (e) {
        if (!cancelled) setRunError(e instanceof Error ? e.message : String(e));
      }
      if (!cancelled) timer = setTimeout(() => void refresh(), 3000);
    };
    void refresh();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [revision]);

  const selectSop = (next: string | null, nextRun?: string) => {
    void navigate(
      next === null
        ? '/sops/new'
        : `/sops/${encodeURIComponent(next)}${nextRun ? `?${new URLSearchParams({ run: nextRun })}` : ''}`,
    );
    if (!window.matchMedia('(min-width: 1280px)').matches) setLibraryOpen(false);
  };
  if (selected !== undefined) {
    const id = editors.current[selectedKey]?.id ?? crypto.randomUUID();
    editors.current[selectedKey] = {
      id,
      element: (
        <SopEditor
          editing={selected}
          runId={runId}
          runs={runs}
          runError={runError}
          onSelectRun={(sop, run) => selectSop(sop, run)}
          onEdit={() => selectSop(selected)}
          libraryOpen={libraryOpen}
          onToggleLibrary={() => setLibraryOpen((open) => !open)}
          onSaved={(savedName) => {
            // Keep the new SOP's mounted helper when its draft acquires a saved name.
            if (selected === null && editors.current['@new']) {
              editors.current[savedName] = editors.current['@new'];
              delete editors.current['@new'];
              lastSelection.current = savedName;
            }
            setRevision((value) => value + 1);
            void navigate(`/sops/${encodeURIComponent(savedName)}`, {
              replace: true,
            });
          }}
        />
      ),
    };
  }
  const filtered = sops
    .filter((sop) => `${sop.name} ${sop.description}`.toLowerCase().includes(query.toLowerCase()))
    .sort((a, b) => a.name.localeCompare(b.name));

  return (
    <div className="relative flex h-full min-h-0 min-w-0 overflow-hidden">
      {libraryOpen && (
        <>
          <button
            type="button"
            aria-label={t('sop_workspace.close_library')}
            className="absolute inset-0 z-20 bg-black/40 xl:hidden"
            onClick={() => setLibraryOpen(false)}
          />
          <aside
            aria-label={t('sop_workspace.library')}
            className="absolute inset-y-0 left-0 z-30 flex w-64 max-w-[85vw] shrink-0 flex-col border-r border-pc-border bg-pc-surface xl:static xl:z-auto"
          >
            <div className="flex items-center justify-between px-4 py-4">
              <span className="flex items-center gap-2 text-sm font-medium">
                <Workflow className="h-4 w-4 text-pc-accent" />
                {t('sop_workspace.library')}
              </span>
              <button
                type="button"
                aria-label={t('sop_workspace.close_library')}
                onClick={() => setLibraryOpen(false)}
                className="rounded-md p-1.5 text-pc-text-muted hover:bg-pc-elevated"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="space-y-3 px-3 pb-3">
              <button
                type="button"
                onClick={() => selectSop(null)}
                className="btn-primary flex w-full items-center justify-center gap-2 px-3 py-2 text-sm"
              >
                <Plus className="h-4 w-4" />
                {t('sops.new')}
              </button>
              <label className="flex items-center gap-2 rounded-lg border border-pc-border px-2.5 py-2 text-pc-text-muted">
                <Search className="h-3.5 w-3.5 shrink-0" />
                <input
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  aria-label={t('workspace.find_sop')}
                  placeholder={t('workspace.find_sop')}
                  className="w-full min-w-0 bg-transparent text-xs outline-none"
                />
              </label>
            </div>
            <nav
              aria-label={t('sop_workspace.library')}
              className="min-h-0 flex-1 space-y-1 overflow-y-auto px-2 pb-4"
            >
              {filtered.map((sop) => (
                <button
                  key={sop.name}
                  type="button"
                  aria-current={selected === sop.name ? 'page' : undefined}
                  onClick={() => selectSop(sop.name)}
                  className={`w-full rounded-lg px-3 py-3 text-left ${selected === sop.name ? 'bg-pc-accent/10 text-pc-accent' : 'text-pc-text hover:bg-pc-elevated'}`}
                >
                  <span className="block truncate text-sm font-medium">{sop.name}</span>
                  <span className="mt-1 block truncate text-xs text-pc-text-muted">
                    {sop.description || t('workspace.sop_definition')}
                  </span>
                </button>
              ))}
              {loaded && !filtered.length && (
                <p className="p-3 text-xs text-pc-text-muted">
                  {t(sops.length ? 'nav.cmdk.empty' : 'sops.empty')}
                </p>
              )}
            </nav>
            {error && (
              <div role="alert" className="border-t border-pc-border p-3 text-xs text-status-error">
                {error}
                <button
                  type="button"
                  onClick={() => setRevision((v) => v + 1)}
                  className="mt-2 block underline"
                >
                  {t('common.retry')}
                </button>
              </div>
            )}
          </aside>
        </>
      )}
      <div className="min-w-0 flex-1">
        {Object.entries(editors.current).map(([key, entry]) => (
          <div key={entry.id} hidden={key !== selectedKey} className="h-full">
            <VisibleWorkspace.Provider value={visible && key === selectedKey}>
              {entry.element}
            </VisibleWorkspace.Provider>
          </div>
        ))}
        {selected === undefined && (
          <div className="flex h-full flex-col">
            <div className="border-b border-pc-border p-3">
              <button
                type="button"
                onClick={() => setLibraryOpen((open) => !open)}
                aria-label={t('sop_workspace.toggle_library')}
                aria-expanded={libraryOpen}
                className="rounded-lg p-2 hover:bg-pc-elevated"
              >
                <PanelLeft className="h-4 w-4" />
              </button>
            </div>
            <div className="m-auto max-w-md px-8 py-12 text-center">
              <Workflow className="mx-auto mb-5 h-9 w-9 text-pc-accent" />
              <h1 className="text-xl font-semibold">{t('sop_workspace.empty_title')}</h1>
              <p className="mt-3 text-sm leading-relaxed text-pc-text-muted">
                {t('sop_workspace.empty_hint')}
              </p>
              {error && (
                <p role="alert" className="mt-4 text-sm text-status-error">
                  {error}
                </p>
              )}
              {!loaded ? (
                <p role="status" className="mt-6 text-sm">
                  {t('common.loading')}
                </p>
              ) : (
                <button
                  type="button"
                  onClick={() => selectSop(null)}
                  className="btn-primary mt-6 inline-flex items-center gap-2 px-4 py-2 text-sm"
                >
                  <Plus className="h-4 w-4" />
                  {t('sops.new')}
                </button>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
