// One-level directory browser scoped to `<install>/shared/`. Backs the
// skill-bundle directory field on /config/skill-bundles/<alias>. Opens
// inside a popover anchored to the input; lists folders + files for the
// current path, lets the operator step in/out, and writes the relative
// path back to the field on selection. The actual containment + sorting
// rules live in `zeroclaw_runtime::browse::list_directory`; this
// component is presentation-only.

import { useEffect, useRef, useState } from 'react';
import { ArrowUp, FolderOpen, ChevronRight, RefreshCw, FolderPlus, Trash2 } from 'lucide-react';
import { Button, ConfirmDialog } from '@/components/ui';
import { t } from '@/lib/i18n';
import {
  ApiError,
  browseShared,
  mkdirShared,
  rmdirShared,
  type BrowseEntry,
} from '../../lib/api';

interface DirectoryPickerProps {
  /** Current relative path (empty = `shared/`). */
  value: string;
  /** Called when the operator selects a directory. */
  onSelect: (path: string) => void;
  /** Called when the popover requests close (Cancel / outside). */
  onClose: () => void;
}

export default function DirectoryPicker({ value, onSelect, onClose }: DirectoryPickerProps) {
  const [cwd, setCwd] = useState<string>(initialCwd(value));
  const [entries, setEntries] = useState<BrowseEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadTick, setReloadTick] = useState(0);
  const [creating, setCreating] = useState(false);
  const [newDirName, setNewDirName] = useState('');
  const [busyDir, setBusyDir] = useState<string | null>(null);
  // The directory name queued for deletion; non-null opens the confirm dialog.
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // Restore focus to the trigger only on keyboard/explicit close — NOT when
  // the user dismissed by clicking another element (outside-click), where
  // restoring would steal focus from whatever they just clicked.
  const restoreFocusRef = useRef(true);

  // Focus the popover panel on open so keyboard users land inside it, and
  // restore focus to the trigger when it closes (unless dismissed by an
  // outside click).
  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    panelRef.current?.focus();
    return () => {
      if (restoreFocusRef.current) previouslyFocused?.focus?.();
    };
  }, []);

  // Esc dismisses the picker; outside (backdrop-equivalent) clicks dismiss it
  // too. The inline "new folder" input owns Esc while open, and the delete
  // confirm dialog owns it while open, so only close the whole picker from Esc
  // when neither is active.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !creating && pendingDelete === null) {
        e.stopPropagation();
        onClose();
      }
    };
    const onPointerDown = (e: PointerEvent) => {
      // Ignore clicks on the trigger that opened us — it owns its own toggle,
      // and closing here would race its onClick and reopen the picker.
      const target = e.target as Element | null;
      if (target?.closest?.('[data-dirpicker-trigger]')) return;
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        restoreFocusRef.current = false;
        onClose();
      }
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onPointerDown);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onPointerDown);
    };
  }, [onClose, creating, pendingDelete]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    browseShared(cwd)
      .then((r) => {
        if (cancelled) return;
        setEntries(r.entries);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(
          e instanceof ApiError
            ? `[${e.envelope.code}] ${e.envelope.message}`
            : e instanceof Error
              ? e.message
              : String(e),
        );
      })
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [cwd, reloadTick]);

  const reload = () => setReloadTick((n) => n + 1);

  const handleCreate = async () => {
    const name = newDirName.trim();
    if (!name) return;
    if (name.includes('/') || name.includes('\\')) {
      setError(t('dir_picker.name_no_slashes'));
      return;
    }
    const target = cwd ? `${cwd}/${name}` : name;
    setError(null);
    try {
      await mkdirShared(target);
      setCreating(false);
      setNewDirName('');
      reload();
    } catch (e) {
      setError(
        e instanceof ApiError
          ? `[${e.envelope.code}] ${e.envelope.message}`
          : e instanceof Error
            ? e.message
            : String(e),
      );
    }
  };

  // Run the actual delete once the operator confirms via the dialog.
  const confirmDelete = async (name: string) => {
    const target = cwd ? `${cwd}/${name}` : name;
    setBusyDir(name);
    setError(null);
    try {
      await rmdirShared(target);
      reload();
    } catch (e) {
      setError(
        e instanceof ApiError
          ? `[${e.envelope.code}] ${e.envelope.message}`
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      setBusyDir(null);
    }
  };

  const parent = (() => {
    if (!cwd) return null;
    const idx = cwd.lastIndexOf('/');
    return idx <= 0 ? '' : cwd.slice(0, idx);
  })();

  const enterDir = (name: string) => {
    setCwd(cwd ? `${cwd}/${name}` : name);
  };

  return (
    <div
      ref={panelRef}
      tabIndex={-1}
      className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface shadow-[var(--pc-shadow-md)] overflow-hidden focus:outline-none"
      role="dialog"
      aria-label={t('dir_picker.aria_label')}
    >
      <div className="flex items-center gap-2 px-3 py-2 border-b border-pc-border text-xs text-pc-text-secondary">
        <FolderOpen className="h-3.5 w-3.5 flex-shrink-0" />
        <code className="flex-1 min-w-0 truncate text-pc-text">
          shared/{cwd}
        </code>
        <button
          type="button"
          onClick={() => setCreating((v) => !v)}
          title={t('dir_picker.new_folder_here')}
          aria-label={t('dir_picker.new_folder_here')}
          className="h-6 w-6 inline-flex items-center justify-center rounded-[var(--radius-sm)] text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-surface"
        >
          <FolderPlus className="h-3.5 w-3.5" />
        </button>
        <button
          type="button"
          onClick={reload}
          title={t('common.refresh')}
          aria-label={t('common.refresh')}
          className="h-6 w-6 inline-flex items-center justify-center rounded-[var(--radius-sm)] text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-surface"
        >
          <RefreshCw className="h-3.5 w-3.5" />
        </button>
      </div>

      {creating && (
        <div className="flex items-center gap-2 px-3 py-2 border-b border-pc-border">
          <input
            type="text"
            value={newDirName}
            onChange={(e) => setNewDirName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void handleCreate();
              if (e.key === 'Escape') {
                setCreating(false);
                setNewDirName('');
              }
            }}
            placeholder={t('dir_picker.new_folder_placeholder')}
            className="input-electric flex-1 px-2 py-1 text-xs"
            autoFocus
          />
          <Button
            size="sm"
            variant="primary"
            onClick={() => void handleCreate()}
            disabled={!newDirName.trim()}
          >
            {t('dir_picker.create')}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setCreating(false);
              setNewDirName('');
            }}
          >
            {t('common.cancel')}
          </Button>
        </div>
      )}

      <ul className="max-h-72 overflow-y-auto divide-y divide-pc-border">
        {parent !== null && (
          <li>
            <button
              type="button"
              onClick={() => setCwd(parent)}
              className="w-full flex items-center gap-2 px-3 py-2 text-sm text-left text-pc-text-secondary transition-colors hover:bg-[var(--pc-hover)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--pc-focus)]"
            >
              <ArrowUp className="h-3.5 w-3.5 flex-shrink-0" />
              {t('dir_picker.up_one_level')}
            </button>
          </li>
        )}
        {loading ? (
          <li className="px-3 py-6 flex items-center justify-center">
            <div
              className="h-5 w-5 border-2 rounded-full animate-spin"
              style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
            />
          </li>
        ) : error ? (
          <li className="px-3 py-3 text-xs text-status-error">
            {error}
          </li>
        ) : entries.length === 0 ? (
          <li className="px-3 py-3 text-xs italic text-pc-text-faint">
            {t('dir_picker.empty')}
          </li>
        ) : (
          entries.map((entry) => (
            <li key={`${entry.kind}-${entry.name}`}>
              {entry.kind === 'dir' ? (
                <div className="flex items-stretch">
                  <button
                    type="button"
                    onClick={() => enterDir(entry.name)}
                    className="flex-1 flex items-center gap-2 px-3 py-2 text-sm text-left text-pc-text transition-colors hover:bg-[var(--pc-hover)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--pc-focus)]"
                  >
                    <FolderOpen className="h-3.5 w-3.5 flex-shrink-0 text-pc-accent" />
                    <span className="flex-1 min-w-0 truncate">{entry.name}</span>
                    <ChevronRight className="h-3.5 w-3.5 flex-shrink-0 text-pc-text-muted" />
                  </button>
                  <button
                    type="button"
                    onClick={() => setPendingDelete(entry.name)}
                    disabled={busyDir === entry.name}
                    title={`${t('dir_picker.delete_prefix')}shared/${cwd ? `${cwd}/` : ''}${entry.name}`}
                    aria-label={`${t('dir_picker.delete_prefix')}shared/${cwd ? `${cwd}/` : ''}${entry.name}`}
                    className="px-2 text-status-error opacity-60 transition-colors hover:opacity-100 hover:bg-status-error/10 disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--pc-focus)]"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              ) : (
                <div className="flex items-center gap-2 px-3 py-2 text-sm text-pc-text-muted">
                  <span className="h-3.5 w-3.5 flex-shrink-0" />
                  <span className="flex-1 min-w-0 truncate">{entry.name}</span>
                  {typeof entry.size === 'number' && (
                    <span className="text-xs text-pc-text-faint">
                      {formatBytes(entry.size)}
                    </span>
                  )}
                </div>
              )}
            </li>
          ))
        )}
      </ul>

      <div className="flex items-center justify-between gap-2 px-3 py-2 border-t border-pc-border">
        <span className="text-xs text-pc-text-faint">
          {t('dir_picker.relative_hint_prefix')}<code>shared/</code>{t('dir_picker.relative_hint_suffix')}
        </span>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="ghost" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button
            size="sm"
            variant="primary"
            onClick={() => onSelect(cwd ? `shared/${cwd}` : 'shared')}
            title={t('dir_picker.use_this_title')}
          >
            {t('dir_picker.use_this')}
          </Button>
        </div>
      </div>

      {/* Themed confirm for the destructive directory delete. Rendered inside
          the picker panel so its backdrop/clicks are treated as in-bounds by
          the outside-click handler above and don't dismiss the picker. */}
      <ConfirmDialog
        open={pendingDelete !== null}
        danger
        title={t('dir_picker.delete_dialog_title')}
        message={`${t('dir_picker.delete_prefix')}shared/${
          cwd ? `${cwd}/${pendingDelete}` : pendingDelete
        }${t('dir_picker.delete_dialog_suffix')}`}
        confirmLabel={t('common.delete')}
        onConfirm={() => {
          if (pendingDelete !== null) void confirmDelete(pendingDelete);
          setPendingDelete(null);
        }}
        onClose={() => setPendingDelete(null)}
      />
    </div>
  );
}

function initialCwd(value: string): string {
  // Field stores `shared/skills/<alias>/` or similar; strip the `shared/`
  // prefix so the API call (which is implicitly relative to `shared/`)
  // doesn't double-traverse.
  const trimmed = value.trim().replace(/^\.\//, '').replace(/\/+$/, '');
  if (trimmed.startsWith('shared/')) return trimmed.slice('shared/'.length);
  if (trimmed === 'shared') return '';
  return '';
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}
