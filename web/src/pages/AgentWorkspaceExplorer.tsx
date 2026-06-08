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
        <Link
          to={`/config/agents/${encodeURIComponent(alias)}`}
          className="btn-secondary inline-flex items-center gap-2 text-sm px-3 py-1.5"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to {alias}
        </Link>
        <h1
          className="text-lg font-semibold"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          Workspace
        </h1>
        <code
          className="text-xs font-mono truncate"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          agents/{alias}/workspace/{cwd}
        </code>
        <div className="ml-auto inline-flex items-center gap-2">
          <button
            type="button"
            onClick={() => void createDirectory()}
            className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-1.5"
            title="Create a new folder in the current directory"
          >
            <FolderPlus className="h-4 w-4" />
            New folder
          </button>
          <button
            type="button"
            onClick={() => setReloadTick((n) => n + 1)}
            className="btn-icon"
            title="Refresh"
          >
            <RefreshCw className="h-4 w-4" />
          </button>
        </div>
      </div>

      {error && (
        <div
          className="rounded-xl border p-3 text-sm"
          style={{
            background: 'var(--color-status-error-alpha-08)',
            borderColor: 'var(--color-status-error-alpha-20)',
            color: 'var(--color-status-error)',
          }}
        >
          {error}
        </div>
      )}

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        <div
          className="rounded-xl border overflow-hidden lg:col-span-1"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          <ul
            className="max-h-[70vh] overflow-y-auto divide-y"
            style={{ borderColor: 'var(--pc-border)' }}
          >
            {parent !== null && (
              <li>
                <button
                  type="button"
                  onClick={() => setCwd(parent)}
                  className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left hover:opacity-90"
                  style={{ color: 'var(--pc-text-secondary)' }}
                >
                  <ArrowUp className="h-3.5 w-3.5 flex-shrink-0" />
                  .. (up one level)
                </button>
              </li>
            )}
            {loading ? (
              <li className="px-3 py-6 flex items-center justify-center">
                <div
                  className="h-5 w-5 border-2 rounded-full animate-spin"
                  style={{
                    borderColor: 'var(--pc-border)',
                    borderTopColor: 'var(--pc-accent)',
                  }}
                />
              </li>
            ) : entries.length === 0 ? (
              <li
                className="px-3 py-3 text-xs italic"
                style={{ color: 'var(--pc-text-faint)' }}
              >
                (empty)
              </li>
            ) : (
              entries.map((entry) => {
                const full = cwd ? `${cwd}/${entry.name}` : entry.name;
                const isSelected = selected === full && entry.kind === 'file';
                return (
                  <li key={`${entry.kind}-${entry.name}`}>
                    <div
                      className="flex items-stretch"
                      style={
                        isSelected
                          ? { background: 'var(--pc-accent-glow)' }
                          : undefined
                      }
                    >
                      <button
                        type="button"
                        onClick={() => {
                          if (entry.kind === 'dir') {
                            setCwd(full);
                          } else {
                            void openFile(entry.name);
                          }
                        }}
                        className="flex-1 flex items-center gap-2 px-3 py-2 text-sm text-left hover:opacity-90 min-w-0"
                        style={{ color: 'var(--pc-text-primary)' }}
                      >
                        {entry.kind === 'dir' ? (
                          <FolderOpen
                            className="h-3.5 w-3.5 flex-shrink-0"
                            style={{ color: 'var(--pc-accent)' }}
                          />
                        ) : (
                          <FileText
                            className="h-3.5 w-3.5 flex-shrink-0"
                            style={{ color: 'var(--pc-text-muted)' }}
                          />
                        )}
                        <span className="flex-1 min-w-0 truncate">{entry.name}</span>
                        {entry.kind === 'file' && typeof entry.size === 'number' && (
                          <span
                            className="text-xs flex-shrink-0"
                            style={{ color: 'var(--pc-text-faint)' }}
                          >
                            {formatBytes(entry.size)}
                          </span>
                        )}
                      </button>
                      {entry.protected ? (
                        <span
                          className="px-2 flex items-center"
                          title="Protected — owned by the runtime, cannot be renamed or deleted from the dashboard"
                          style={{ color: 'var(--pc-text-faint)' }}
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
                            className="px-2 opacity-60 hover:opacity-100 disabled:opacity-30"
                            style={{ color: 'var(--pc-text-muted)' }}
                          >
                            <Edit2 className="h-3.5 w-3.5" />
                          </button>
                          <button
                            type="button"
                            onClick={() => void deletePath(entry.name, entry.kind)}
                            disabled={busy === full}
                            title="Delete"
                            className="px-2 opacity-60 hover:opacity-100 disabled:opacity-30"
                            style={{ color: 'var(--color-status-error)' }}
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
        </div>

        <div
          className="rounded-xl border overflow-hidden lg:col-span-2 flex flex-col"
          style={{
            borderColor: 'var(--pc-border)',
            background: 'var(--pc-bg-surface)',
            minHeight: '60vh',
          }}
        >
          {selected ? (
            <>
              <div
                className="flex items-center gap-2 px-4 py-2 border-b text-xs"
                style={{
                  borderColor: 'var(--pc-border)',
                  color: 'var(--pc-text-secondary)',
                }}
              >
                <FileText className="h-3.5 w-3.5 flex-shrink-0" />
                <code
                  className="flex-1 min-w-0 truncate font-mono"
                  style={{ color: 'var(--pc-text-primary)' }}
                >
                  {selected}
                </code>
                {viewer && (
                  <span style={{ color: 'var(--pc-text-faint)' }}>
                    {formatBytes(viewer.size)} · {viewer.encoding}
                  </span>
                )}
              </div>
              <div className="flex-1 overflow-auto p-4">
                {viewerLoading ? (
                  <div
                    className="h-5 w-5 border-2 rounded-full animate-spin"
                    style={{
                      borderColor: 'var(--pc-border)',
                      borderTopColor: 'var(--pc-accent)',
                    }}
                  />
                ) : viewerError ? (
                  <p
                    className="text-sm"
                    style={{ color: 'var(--color-status-error)' }}
                  >
                    {viewerError}
                  </p>
                ) : viewer ? (
                  viewer.is_text ? (
                    <pre
                      className="text-xs font-mono whitespace-pre-wrap break-words"
                      style={{ color: 'var(--pc-text-primary)' }}
                    >
                      {viewer.content}
                    </pre>
                  ) : (
                    <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
                      Binary file ({formatBytes(viewer.size)}). Preview is base64-
                      encoded; download via CLI to inspect.
                    </p>
                  )
                ) : null}
              </div>
            </>
          ) : (
            <div
              className="flex-1 flex items-center justify-center text-sm"
              style={{ color: 'var(--pc-text-faint)' }}
            >
              Select a file to view its contents.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
