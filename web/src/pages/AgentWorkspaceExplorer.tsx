import { useEffect, useMemo, useState } from 'react';
import { Link, useParams } from 'react-router-dom';
import {
  ArrowLeft,
  ArrowUp,
  Edit2,
  FileText,
  FolderOpen,
  FolderPlus,
  Lock,
  RefreshCw,
  Trash2,
} from 'lucide-react';
import {
  ApiError,
  createAgentWorkspaceDirectory,
  deleteAgentWorkspacePath,
  listAgentWorkspace,
  moveAgentWorkspacePath,
  readAgentWorkspaceFile,
  type AgentWorkspaceFileRead,
  type BrowseEntry,
} from '@/lib/api';
import { Button, Card } from '@/components/ui';
import { t } from '@/lib/i18n';

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function describeError(e: unknown): string {
  if (e instanceof ApiError) {
    return `[${e.envelope.code}] ${e.envelope.message}`;
  }
  return e instanceof Error ? e.message : String(e);
}

export default function AgentWorkspaceExplorer() {
  const { alias = '' } = useParams<{ alias: string }>();
  const [cwd, setCwd] = useState('');
  const [entries, setEntries] = useState<BrowseEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadTick, setReloadTick] = useState(0);
  const [selected, setSelected] = useState<string | null>(null);
  const [viewer, setViewer] = useState<AgentWorkspaceFileRead | null>(null);
  const [viewerLoading, setViewerLoading] = useState(false);
  const [viewerError, setViewerError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    if (!alias) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    listAgentWorkspace(alias, cwd)
      .then((r) => {
        if (cancelled) return;
        setEntries(r.entries);
      })
      .catch((e) => {
        if (!cancelled) setError(describeError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [alias, cwd, reloadTick]);

  const parent = useMemo(() => {
    if (!cwd) return null;
    const idx = cwd.lastIndexOf('/');
    return idx <= 0 ? '' : cwd.slice(0, idx);
  }, [cwd]);

  const openFile = async (name: string) => {
    const full = cwd ? `${cwd}/${name}` : name;
    setSelected(full);
    setViewer(null);
    setViewerLoading(true);
    setViewerError(null);
    try {
      const r = await readAgentWorkspaceFile(alias, full);
      setViewer(r);
    } catch (e) {
      setViewerError(describeError(e));
    } finally {
      setViewerLoading(false);
    }
  };

  const deletePath = async (name: string, kind: 'dir' | 'file') => {
    const full = cwd ? `${cwd}/${name}` : name;
    if (
      !window.confirm(
        `Delete ${kind === 'dir' ? 'directory' : 'file'} "${full}" from ${alias}'s workspace? ${
          kind === 'dir' ? 'Everything inside it goes too.' : ''
        } This cannot be undone.`,
      )
    ) {
      return;
    }
    setBusy(full);
    setError(null);
    try {
      await deleteAgentWorkspacePath(alias, full);
      if (selected === full) {
        setSelected(null);
        setViewer(null);
      }
      setReloadTick((n) => n + 1);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  };

  const createDirectory = async () => {
    const name = window.prompt(
      `New folder name (under agents/${alias}/workspace/${cwd ? `${cwd}/` : ''}):`,
      '',
    );
    if (!name) return;
    const trimmed = name.trim().replace(/^\/+|\/+$/g, '');
    if (!trimmed) return;
    if (trimmed.includes('..')) {
      setError("Folder name cannot contain '..'");
      return;
    }
    const full = cwd ? `${cwd}/${trimmed}` : trimmed;
    setBusy(full);
    setError(null);
    try {
      await createAgentWorkspaceDirectory(alias, full);
      setReloadTick((n) => n + 1);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  };

  const renamePath = async (name: string) => {
    const from = cwd ? `${cwd}/${name}` : name;
    const next = window.prompt(`Rename "${name}" to:`, name);
    if (!next || next === name) return;
    if (next.includes('..')) {
      setError("Rename target cannot contain '..'");
      return;
    }
    const to = cwd ? `${cwd}/${next}` : next;
    setBusy(from);
    setError(null);
    try {
      await moveAgentWorkspacePath(alias, from, to);
      if (selected === from) setSelected(to);
      setReloadTick((n) => n + 1);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="p-6 space-y-4 animate-fade-in">
      <div className="flex items-center gap-3 flex-wrap">
        <Link to={`/config/agents/${encodeURIComponent(alias)}`} className="inline-block">
          <Button variant="ghost" size="sm">
            <ArrowLeft className="h-4 w-4" />
            {t('common.back')} to {alias}
          </Button>
        </Link>
        <h1 className="text-lg font-semibold text-pc-text">
          Workspace
        </h1>
        <code className="text-xs font-mono truncate text-pc-text-muted">
          agents/{alias}/workspace/{cwd}
        </code>
        <div className="ml-auto inline-flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void createDirectory()}
            title="Create a new folder in the current directory"
          >
            <FolderPlus className="h-4 w-4" />
            New folder
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setReloadTick((n) => n + 1)}
            className="w-7 px-0"
            title={t('common.refresh')}
            aria-label={t('common.refresh')}
          >
            <RefreshCw className="h-4 w-4" />
          </Button>
        </div>
      </div>

      {error && (
        <div className="rounded-[var(--radius-md)] border border-status-error/20 bg-status-error/10 text-status-error p-3 text-sm">
          {error}
        </div>
      )}

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        <Card padded={false} className="overflow-hidden lg:col-span-1">
          <ul className="max-h-[70vh] overflow-y-auto divide-y divide-pc-border">
            {parent !== null && (
              <li>
                <button
                  type="button"
                  onClick={() => setCwd(parent)}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left text-pc-text-secondary hover:bg-[var(--pc-hover)] transition-colors"
                >
                  <ArrowUp className="h-3.5 w-3.5 flex-shrink-0" />
                  .. (up one level)
                </button>
              </li>
            )}
            {loading ? (
              <li className="px-3 py-6 flex items-center justify-center">
                <div className="h-5 w-5 border-2 rounded-full animate-spin border-pc-border border-t-pc-accent" />
              </li>
            ) : entries.length === 0 ? (
              <li className="px-3 py-3 text-xs italic text-pc-text-faint">
                (empty)
              </li>
            ) : (
              entries.map((entry) => {
                const full = cwd ? `${cwd}/${entry.name}` : entry.name;
                const isSelected = selected === full && entry.kind === 'file';
                return (
                  <li key={`${entry.kind}-${entry.name}`}>
                    <div className={`flex items-stretch transition-colors ${isSelected ? 'bg-pc-accent/10' : 'hover:bg-[var(--pc-hover)]'}`}>
                      <button
                        type="button"
                        onClick={() => {
                          if (entry.kind === 'dir') {
                            setCwd(full);
                          } else {
                            void openFile(entry.name);
                          }
                        }}
                        className="flex-1 flex items-center gap-2 px-3 py-2 text-sm text-left text-pc-text min-w-0"
                      >
                        {entry.kind === 'dir' ? (
                          <FolderOpen className="h-3.5 w-3.5 flex-shrink-0 text-pc-accent" />
                        ) : (
                          <FileText className="h-3.5 w-3.5 flex-shrink-0 text-pc-text-muted" />
                        )}
                        <span className="flex-1 min-w-0 truncate">{entry.name}</span>
                        {entry.kind === 'file' && typeof entry.size === 'number' && (
                          <span className="text-xs flex-shrink-0 text-pc-text-faint">
                            {formatBytes(entry.size)}
                          </span>
                        )}
                      </button>
                      {entry.protected ? (
                        <span
                          className="px-2 flex items-center text-pc-text-faint"
                          title="Protected — owned by the runtime, cannot be renamed or deleted from the dashboard"
                        >
                          <Lock className="h-3.5 w-3.5" />
                        </span>
                      ) : (
                        <>
                          <button
                            type="button"
                            onClick={() => void renamePath(entry.name)}
                            disabled={busy === full}
                            title="Rename / move"
                            className="px-2 text-pc-text-muted hover:text-pc-text transition-colors disabled:opacity-30"
                          >
                            <Edit2 className="h-3.5 w-3.5" />
                          </button>
                          <button
                            type="button"
                            onClick={() => void deletePath(entry.name, entry.kind)}
                            disabled={busy === full}
                            title={t('common.delete')}
                            className="px-2 text-pc-text-muted hover:text-status-error transition-colors disabled:opacity-30"
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </button>
                        </>
                      )}
                    </div>
                  </li>
                );
              })
            )}
          </ul>
        </Card>

        <Card
          padded={false}
          className="overflow-hidden lg:col-span-2 flex flex-col"
          style={{ minHeight: '60vh' }}
        >
          {selected ? (
            <>
              <div className="flex items-center gap-2 px-4 py-2 border-b border-pc-border text-xs text-pc-text-secondary bg-pc-elevated">
                <FileText className="h-3.5 w-3.5 flex-shrink-0" />
                <code className="flex-1 min-w-0 truncate font-mono text-pc-text">
                  {selected}
                </code>
                {viewer && (
                  <span className="text-pc-text-faint">
                    {formatBytes(viewer.size)} · {viewer.encoding}
                  </span>
                )}
              </div>
              <div className="flex-1 overflow-auto p-4">
                {viewerLoading ? (
                  <div className="h-5 w-5 border-2 rounded-full animate-spin border-pc-border border-t-pc-accent" />
                ) : viewerError ? (
                  <p className="text-sm text-status-error">
                    {viewerError}
                  </p>
                ) : viewer ? (
                  viewer.is_text ? (
                    <pre className="text-xs font-mono whitespace-pre-wrap break-words text-pc-text">
                      {viewer.content}
                    </pre>
                  ) : (
                    <p className="text-sm text-pc-text-muted">
                      Binary file ({formatBytes(viewer.size)}). Preview is base64-
                      encoded; download via CLI to inspect.
                    </p>
                  )
                ) : null}
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center text-sm text-pc-text-faint">
              Select a file to view its contents.
            </div>
          )}
        </Card>
      </div>
    </div>
  );
}
