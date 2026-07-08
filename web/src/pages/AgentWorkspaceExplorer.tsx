import { useEffect, useId, useMemo, useRef, useState } from 'react';
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
import { Button, Card, ConfirmDialog } from '@/components/ui';
import { t } from '@/lib/i18n';

/**
 * Minimal in-app prompt modal — a token-themed, focus-trapped replacement for
 * `window.prompt`. Mirrors the modal conventions in `AliasPromptDialog`.
 */
function PromptDialog({
  open,
  title,
  message,
  initialValue = '',
  placeholder,
  confirmLabel = t('common.confirm'),
  onConfirm,
  onClose,
}: {
  open: boolean;
  title: string;
  message?: string;
  initialValue?: string;
  placeholder?: string;
  confirmLabel?: string;
  onConfirm: (value: string) => void;
  onClose: () => void;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [value, setValue] = useState(initialValue);
  const titleId = useId();

  // Reset the field to the supplied initial value each time the dialog opens.
  useEffect(() => {
    if (open) setValue(initialValue);
  }, [open, initialValue]);

  // Focus + select the input on open; restore focus to the trigger on close.
  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    inputRef.current?.select();
    return () => previouslyFocused?.focus?.();
  }, [open]);

  // Esc closes; Tab is trapped within the panel.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key !== 'Tab') return;
      const panel = panelRef.current;
      if (!panel) return;
      const focusable = Array.from(
        panel.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((el) => el.offsetParent !== null || el === document.activeElement);
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      const active = document.activeElement;
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={onClose}
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative w-full max-w-sm mx-4 rounded-[var(--radius-xl)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="px-6 pt-5 pb-4 flex flex-col gap-3">
          <h2 id={titleId} className="text-sm font-semibold text-pc-text">
            {title}
          </h2>
          {message && <p className="text-xs text-pc-text-muted">{message}</p>}
          <input
            ref={inputRef}
            type="text"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') onConfirm(value);
            }}
            placeholder={placeholder}
            className="input-electric w-full px-3 py-2 text-sm"
          />
        </div>
        <div className="flex items-center justify-end gap-2 px-6 py-4 border-t border-pc-border">
          <Button variant="ghost" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button variant="primary" onClick={() => onConfirm(value)}>
            {confirmLabel}
          </Button>
        </div>
      </div>
    </div>
  );
}

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
  // Path queued for deletion (opens the confirm dialog); kind tells the copy.
  const [pendingDelete, setPendingDelete] = useState<{ name: string; kind: 'dir' | 'file' } | null>(null);
  // Whether the "new folder" prompt is open.
  const [creatingDir, setCreatingDir] = useState(false);
  // Entry name queued for rename (opens the rename prompt).
  const [renaming, setRenaming] = useState<string | null>(null);

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

  const deletePath = async (name: string) => {
    const full = cwd ? `${cwd}/${name}` : name;
    setPendingDelete(null);
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

  const createDirectory = async (name: string) => {
    const trimmed = name.trim().replace(/^\/+|\/+$/g, '');
    if (!trimmed) {
      setCreatingDir(false);
      return;
    }
    if (trimmed.includes('..')) {
      setCreatingDir(false);
      setError(t('workspace.error_folder_name_dotdot'));
      return;
    }
    setCreatingDir(false);
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

  const renamePath = async (name: string, next: string) => {
    const from = cwd ? `${cwd}/${name}` : name;
    if (!next || next === name) {
      setRenaming(null);
      return;
    }
    if (next.includes('..')) {
      setRenaming(null);
      setError(t('workspace.error_rename_dotdot'));
      return;
    }
    setRenaming(null);
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
        <Link to={`/agent/${encodeURIComponent(alias)}`} className="inline-block">
          <Button variant="ghost" size="sm">
            <ArrowLeft className="h-4 w-4" />
            {t('workspace.back_to_chat_prefix')} ({alias})
          </Button>
        </Link>
        <h1 className="text-lg font-semibold text-pc-text">
          {t('workspace.title')}
        </h1>
        <code className="text-xs font-mono truncate text-pc-text-muted">
          agents/{alias}/workspace/{cwd}
        </code>
        <div className="ml-auto inline-flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setCreatingDir(true)}
            title={t('workspace.new_folder_title')}
          >
            <FolderPlus className="h-4 w-4" />
            {t('workspace.new_folder')}
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
                  {t('workspace.up_one_level')}
                </button>
              </li>
            )}
            {loading ? (
              <li className="px-3 py-6 flex items-center justify-center">
                <div className="h-5 w-5 border-2 rounded-full animate-spin border-pc-border border-t-pc-accent" />
              </li>
            ) : entries.length === 0 ? (
              <li className="px-3 py-3 text-xs italic text-pc-text-faint">
                {t('workspace.empty')}
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
                          title={t('workspace.protected_title')}
                        >
                          <Lock className="h-3.5 w-3.5" />
                        </span>
                      ) : (
                        <>
                          <button
                            type="button"
                            onClick={() => setRenaming(entry.name)}
                            disabled={busy === full}
                            title={t('workspace.rename_move_title')}
                            className="px-2 text-pc-text-muted hover:text-pc-text transition-colors disabled:opacity-30"
                          >
                            <Edit2 className="h-3.5 w-3.5" />
                          </button>
                          <button
                            type="button"
                            onClick={() => setPendingDelete({ name: entry.name, kind: entry.kind })}
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
                      {t('workspace.binary_file_prefix')} ({formatBytes(viewer.size)}).{' '}
                      {t('workspace.binary_file_suffix')}
                    </p>
                  )
                ) : null}
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center text-sm text-pc-text-faint">
              {t('workspace.select_file_hint')}
            </div>
          )}
        </Card>
      </div>

      <ConfirmDialog
        open={pendingDelete !== null}
        danger
        title={
          pendingDelete
            ? `${t('workspace.delete_title_prefix')} ${pendingDelete.name}?`
            : t('workspace.delete_title')
        }
        message={
          pendingDelete ? (
            <>
              {t('workspace.delete_message_prefix')}{' '}
              {pendingDelete.kind === 'dir'
                ? t('workspace.kind_directory')
                : t('workspace.kind_file')}{' '}
              <span className="font-mono text-pc-text-secondary">
                {cwd ? `${cwd}/${pendingDelete.name}` : pendingDelete.name}
              </span>{' '}
              {t('workspace.delete_message_from')} {alias}
              {t('workspace.delete_message_workspace_suffix')}
              {pendingDelete.kind === 'dir' && ` ${t('workspace.delete_message_dir_note')}`}{' '}
              {t('workspace.delete_message_undone')}
            </>
          ) : undefined
        }
        confirmLabel={t('common.delete')}
        onConfirm={() => {
          if (pendingDelete) void deletePath(pendingDelete.name);
        }}
        onClose={() => setPendingDelete(null)}
      />

      <PromptDialog
        open={creatingDir}
        title={t('workspace.new_folder')}
        message={`${t('workspace.new_folder_under')} agents/${alias}/workspace/${cwd ? `${cwd}/` : ''}`}
        placeholder={t('workspace.folder_name_placeholder')}
        confirmLabel={t('workspace.create')}
        onConfirm={(value) => void createDirectory(value)}
        onClose={() => setCreatingDir(false)}
      />

      <PromptDialog
        open={renaming !== null}
        title={renaming ? `${t('workspace.rename_title_prefix')} ${renaming}` : t('workspace.rename_title')}
        message={t('workspace.rename_message')}
        initialValue={renaming ?? ''}
        confirmLabel={t('workspace.rename')}
        onConfirm={(value) => {
          if (renaming !== null) void renamePath(renaming, value.trim());
        }}
        onClose={() => setRenaming(null)}
      />
    </div>
  );
}
