import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, ChevronDown, MessagesSquare, Pencil, Plus, Trash2, X } from 'lucide-react';
import { useAgent } from '@/contexts/AgentContext';
import { getSessions } from '@/lib/api';
import { formatRelative } from '@/lib/format';
import { t } from '@/lib/i18n';

/**
 * One row of the picker. Deliberately not the API `Session` shape: the active
 * conversation is listed even before the gateway has persisted a turn for it,
 * and inventing timestamps for that row would misreport it as stored.
 */
interface SessionRow {
  /** Bare session id — what `/ws/chat`, rename and abort all take. */
  id: string;
  name?: string;
  messageCount: number;
  lastActivity: string | null;
  /** False for the active conversation before its first persisted turn. */
  persisted: boolean;
}

function rowLabel(row: SessionRow): string {
  if (row.name) return row.name;
  return `${t('agent.session_untitled')} ${row.id.slice(0, 8)}`;
}

/**
 * Conversation switcher for one agent (issue #7543).
 *
 * The gateway keys history by session id, so a single agent can hold any number
 * of independent conversations; this exposes them — new, switch, rename,
 * delete — from the chat header. Sessions are re-read each time the menu opens
 * rather than cached, so a conversation that gained turns elsewhere shows an
 * honest message count.
 */
export function SessionPicker({ agentAlias }: { agentAlias: string }) {
  const {
    sessionId,
    sessionPersistence,
    startNewSession,
    goToSession,
    removeSession,
    renameConversation,
  } = useAgent();

  // With persistence off the gateway keeps no record of a conversation, so a
  // new one would strand the current transcript with nothing to switch back to.
  // Withhold the controls rather than lose it silently; "Clear all" remains the
  // way to reset context in that configuration.
  const storesConversations = sessionPersistence === true;

  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<SessionRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState('');
  const [renameFailed, setRenameFailed] = useState(false);
  const [confirmingDeleteId, setConfirmingDeleteId] = useState<string | null>(null);
  const [deleteFailed, setDeleteFailed] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  // Only the newest listing may commit. An active delete issues two overlapping
  // loads — handleDelete's trailing reload, plus the one the sessionId change
  // fires when the pane is moved off the deleted conversation. The first still
  // holds the pre-delete `sessionId` in its closure, so if it resolves last its
  // result (which no longer contains that conversation) falls into the
  // "surface the active conversation" branch below and re-synthesizes the row
  // that was just deleted. Selecting that ghost reconnects to a session the
  // gateway no longer has, minting a fresh empty one.
  const loadGenerationRef = useRef(0);

  const load = useCallback(async () => {
    const generation = ++loadGenerationRef.current;
    const superseded = () => generation !== loadGenerationRef.current;
    setLoading(true);
    setLoadFailed(false);
    try {
      const all = await getSessions();
      if (superseded()) return;
      const mine = all
        // Gateway web-chat sessions only, keyed on the storage prefix rather
        // than merely "has no channel": the TUI's chat pane stores sessions as
        // `rpc_<id>` with this same alias stamped and no channel_id, so an
        // absence check would list terminal conversations here. It matters
        // beyond tidiness — every action in this menu addresses a session by
        // its bare `session_id`, and only a `gw_` key round-trips that way
        // through /ws/chat, rename, abort, messages and delete.
        .filter(
          (s) =>
            s.session_key.startsWith('gw_') &&
            s.channel_id === null &&
            s.agent_alias === agentAlias,
        )
        .map<SessionRow>((s) => ({
          id: s.session_id,
          name: s.name,
          messageCount: s.message_count,
          lastActivity: s.last_activity,
          persisted: true,
        }))
        .sort((a, b) => (b.lastActivity ?? '').localeCompare(a.lastActivity ?? ''));

      // The gateway records a session the moment its socket connects, so this
      // covers the window before that and the persistence-disabled case, where
      // the listing is always empty. Surface the conversation anyway so the
      // menu always shows where the operator currently is.
      if (!mine.some((row) => row.id === sessionId)) {
        mine.unshift({
          id: sessionId,
          messageCount: 0,
          lastActivity: null,
          persisted: false,
        });
      }
      setRows(mine);
    } catch {
      if (superseded()) return;
      setLoadFailed(true);
      setRows([{ id: sessionId, messageCount: 0, lastActivity: null, persisted: false }]);
    } finally {
      // A superseded load must not clear the spinner a newer one is still owed.
      if (!superseded()) setLoading(false);
    }
  }, [agentAlias, sessionId]);

  // Load on mount and after every session switch. Opening the menu reloads
  // separately (see the trigger's onClick) to refresh names and message counts.
  useEffect(() => {
    void load();
  }, [load]);

  // Close on outside click, mirroring the model dropdown in the same header.
  useEffect(() => {
    if (!open) return;
    function handleClickOutside(e: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [open]);

  // Escape closes the menu, and returns focus to the trigger so keyboard users
  // are not dropped onto <body>. A row editor handles Escape itself and stops
  // the event, so dismissing an edit does not also close the menu.
  useEffect(() => {
    if (!open) return;
    function handleEscape(e: KeyboardEvent) {
      if (e.key !== 'Escape') return;
      setOpen(false);
      setRenamingId(null);
      setRenameDraft('');
      setRenameFailed(false);
      setConfirmingDeleteId(null);
      triggerRef.current?.focus();
    }
    document.addEventListener('keydown', handleEscape);
    return () => document.removeEventListener('keydown', handleEscape);
  }, [open]);

  const resetRowActions = useCallback(() => {
    setRenamingId(null);
    setRenameDraft('');
    setRenameFailed(false);
    setConfirmingDeleteId(null);
    setDeleteFailed(false);
  }, []);

  const closeMenu = useCallback(() => {
    setOpen(false);
    resetRowActions();
  }, [resetRowActions]);

  const handleNew = useCallback(() => {
    if (startNewSession()) closeMenu();
  }, [startNewSession, closeMenu]);

  const handleSelect = useCallback((id: string) => {
    // Selecting the row already on screen is still a completed picker action.
    // goToSession correctly reports false for that no-op, but the menu should
    // dismiss just as it does after selecting a different conversation.
    if (id === sessionId || goToSession(id)) closeMenu();
  }, [sessionId, goToSession, closeMenu]);

  const commitRename = useCallback(async (id: string) => {
    const name = renameDraft.trim();
    if (!name) {
      resetRowActions();
      return;
    }
    try {
      await renameConversation(id, name);
      resetRowActions();
      await load();
    } catch {
      // Mostly reached when session persistence is disabled, in which case the
      // gateway has no row to name. Keep the editor open and say so rather than
      // silently dropping what the operator typed.
      setRenameFailed(true);
    }
  }, [renameDraft, renameConversation, resetRowActions, load]);

  const handleDelete = useCallback(async (id: string) => {
    try {
      // Tolerates "nothing stored"; rejects only on a real failure, where the
      // transcript survives on the server and the row must not vanish as if it
      // had not.
      await removeSession(id);
    } catch {
      setDeleteFailed(true);
      return;
    }
    resetRowActions();
    await load();
  }, [removeSession, resetRowActions, load]);

  const activeRow = rows.find((row) => row.id === sessionId);
  const activeLabel = activeRow
    ? rowLabel(activeRow)
    : `${t('agent.session_untitled')} ${sessionId.slice(0, 8)}`;

  return (
    <div className="relative" ref={containerRef}>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => {
          if (open) {
            closeMenu();
            return;
          }
          setOpen(true);
          void load();
        }}
        aria-haspopup="menu"
        aria-expanded={open}
        title={t('agent.sessions')}
        className="flex items-center gap-2 px-3 h-7 rounded-[var(--radius-md)] text-xs font-medium border border-pc-border bg-pc-elevated text-pc-text-secondary transition-colors hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
      >
        <MessagesSquare className="h-3.5 w-3.5" />
        <span className="max-w-[160px] truncate">{activeLabel}</span>
        <ChevronDown className="h-3 w-3" />
      </button>

      {open && (
        <div className="absolute right-0 mt-1.5 rounded-[var(--radius-md)] border border-pc-border bg-pc-elevated shadow-[var(--pc-shadow-md)] z-50 py-1 w-[300px] max-h-80 overflow-y-auto">
          {storesConversations ? (
            <button
              type="button"
              onClick={handleNew}
              className="w-full flex items-center gap-2 px-3 py-2 text-xs font-medium text-pc-text transition-colors hover:bg-[var(--pc-hover)]"
            >
              <Plus className="h-3.5 w-3.5 text-pc-accent" />
              {t('agent.session_new')}
            </button>
          ) : (
            <p className="px-3 py-2 text-xs text-pc-text-muted">
              {t('agent.sessions_not_stored')}
            </p>
          )}

          <div className="my-1 border-t border-pc-border" />

          {loading && (
            <p className="px-3 py-2 text-xs text-pc-text-muted">{t('agent.sessions_loading')}</p>
          )}

          {loadFailed && !loading && (
            <p className="px-3 py-2 text-xs text-status-error">{t('agent.sessions_load_failed')}</p>
          )}

          {rows.map((row) => {
            const isActive = row.id === sessionId;

            if (renamingId === row.id) {
              return (
                <div key={row.id} className="px-3 py-2">
                  <input
                    autoFocus
                    value={renameDraft}
                    onChange={(e) => setRenameDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        e.preventDefault();
                        void commitRename(row.id);
                      } else if (e.key === 'Escape') {
                        e.preventDefault();
                        // Cancel the edit only — the menu stays open.
                        e.stopPropagation();
                        resetRowActions();
                      }
                    }}
                    aria-label={t('agent.session_rename')}
                    aria-invalid={renameFailed}
                    aria-describedby={renameFailed ? `session-rename-error-${row.id}` : undefined}
                    className="w-full px-2 h-7 rounded-[var(--radius-md)] text-xs border border-pc-border bg-pc-surface text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
                  />
                  {renameFailed && (
                    <p
                      id={`session-rename-error-${row.id}`}
                      role="alert"
                      className="mt-1 text-[11px] text-status-error"
                    >
                      {t('agent.session_rename_failed')}
                    </p>
                  )}
                  <div className="mt-1.5 flex items-center justify-end gap-1">
                    <button
                      type="button"
                      onClick={resetRowActions}
                      aria-label={t('agent.session_rename_cancel')}
                      className="p-1 rounded-[var(--radius-md)] text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text"
                    >
                      <X className="h-3.5 w-3.5" />
                    </button>
                    <button
                      type="button"
                      onClick={() => void commitRename(row.id)}
                      aria-label={t('agent.session_rename_save')}
                      className="p-1 rounded-[var(--radius-md)] text-pc-accent transition-colors hover:bg-[var(--pc-hover)]"
                    >
                      <Check className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>
              );
            }

            if (confirmingDeleteId === row.id) {
              return (
                <div key={row.id} className="px-3 py-2 bg-status-error/5">
                  <div className="flex items-center justify-between gap-2">
                  {/* Name the target: in a list of similar-looking rows a bare
                      "Delete this conversation?" does not tell the operator
                      which one they are about to destroy. */}
                  <span className="text-xs text-pc-text truncate" title={rowLabel(row)}>
                    {t('agent.session_delete_confirm').replace('{name}', rowLabel(row))}
                  </span>
                  <div className="flex items-center gap-1 shrink-0">
                    <button
                      type="button"
                      onClick={resetRowActions}
                      className="px-2 h-6 rounded-[var(--radius-md)] text-[11px] text-pc-text-secondary transition-colors hover:bg-[var(--pc-hover)]"
                    >
                      {t('agent.session_delete_cancel')}
                    </button>
                    <button
                      type="button"
                      onClick={() => void handleDelete(row.id)}
                      className="px-2 h-6 rounded-[var(--radius-md)] text-[11px] text-status-error bg-status-error/10 transition-colors hover:bg-status-error/15"
                    >
                      {t('agent.session_delete_confirm_action')}
                    </button>
                  </div>
                  </div>
                  {deleteFailed && (
                    <p role="alert" className="mt-1 text-[11px] text-status-error">
                      {t('agent.session_delete_failed')}
                    </p>
                  )}
                </div>
              );
            }

            return (
              <div
                key={row.id}
                className={`group flex items-center gap-1 pr-2 transition-colors ${
                  isActive ? 'bg-pc-accent/10' : 'hover:bg-[var(--pc-hover)]'
                }`}
              >
                <button
                  type="button"
                  onClick={() => handleSelect(row.id)}
                  disabled={!isActive && !storesConversations}
                  className="flex-1 min-w-0 text-left px-3 py-2 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <span
                    className={`block text-xs truncate ${isActive ? 'text-pc-accent' : 'text-pc-text'}`}
                  >
                    {rowLabel(row)}
                  </span>
                  <span className="block text-[11px] text-pc-text-muted truncate">
                    {row.persisted
                      ? `${row.messageCount} ${t('agent.session_messages')} · ${formatRelative(row.lastActivity)}`
                      : t('agent.session_unsaved')}
                  </span>
                </button>

                {/* Rename and delete only exist server-side, and the row
                    actions stay visible on touch, where there is no hover to
                    reveal them (opacity alone would leave them tappable but
                    invisible). */}
                {storesConversations && (
                  <>
                    <button
                      type="button"
                      onClick={() => {
                        setConfirmingDeleteId(null);
                        setRenameFailed(false);
                        setRenamingId(row.id);
                        setRenameDraft(row.name ?? '');
                      }}
                      aria-label={`${t('agent.session_rename')}: ${rowLabel(row)}`}
                      title={t('agent.session_rename')}
                      className="p-1 rounded-[var(--radius-md)] text-pc-text-muted opacity-60 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 hover:text-pc-text"
                    >
                      <Pencil className="h-3.5 w-3.5" />
                    </button>

                    <button
                      type="button"
                      onClick={() => {
                        setRenamingId(null);
                        setConfirmingDeleteId(row.id);
                      }}
                      aria-label={`${t('agent.session_delete')}: ${rowLabel(row)}`}
                      title={t('agent.session_delete')}
                      className="p-1 rounded-[var(--radius-md)] text-pc-text-muted opacity-60 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 hover:text-status-error"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export default SessionPicker;
