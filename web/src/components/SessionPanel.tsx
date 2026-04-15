import { useState, useEffect, useRef, useCallback } from 'react';
import { Plus, Trash2, MessageSquare } from 'lucide-react';
import type { Session } from '@/types/api';
import { getSessions, deleteSession, renameSession } from '@/lib/api';
import { SESSION_STORAGE_KEY } from '@/lib/ws';
import { generateUUID } from '@/lib/uuid';

interface SessionPanelProps {
  currentSessionId: string;
}

/** Truncate a string to maxLen characters, appending ellipsis when clipped. */
function truncate(str: string, maxLen: number): string {
  if (str.length <= maxLen) return str;
  return `${str.slice(0, maxLen)}...`;
}

/** Format a timestamp as a relative time string (e.g. "2m ago", "3h ago"). */
function relativeTime(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const seconds = Math.floor(diff / 1000);
  if (seconds < 60) return 'just now';
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

export default function SessionPanel({ currentSessionId }: SessionPanelProps) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(true);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editValue, setEditValue] = useState('');
  const editInputRef = useRef<HTMLInputElement>(null);

  const fetchSessions = useCallback(async () => {
    try {
      const data = await getSessions();
      // Sort by last_activity descending
      data.sort((a, b) => new Date(b.last_activity).getTime() - new Date(a.last_activity).getTime());
      setSessions(data);
    } catch {
      // Silently fail — sessions are a convenience feature
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchSessions();
    // Refresh session list when session changes elsewhere
    const handler = () => { fetchSessions(); };
    window.addEventListener('zeroclaw-session-change', handler);
    return () => window.removeEventListener('zeroclaw-session-change', handler);
  }, [fetchSessions]);

  // Focus the rename input when editing starts
  useEffect(() => {
    if (editingId && editInputRef.current) {
      editInputRef.current.focus();
      editInputRef.current.select();
    }
  }, [editingId]);

  const handleNewChat = () => {
    const newId = generateUUID();
    sessionStorage.setItem(SESSION_STORAGE_KEY, newId);
    window.dispatchEvent(new CustomEvent('zeroclaw-session-change', { detail: { sessionId: newId } }));
  };

  const handleSelectSession = (sessionId: string) => {
    if (sessionId === currentSessionId) return;
    sessionStorage.setItem(SESSION_STORAGE_KEY, sessionId);
    window.dispatchEvent(new CustomEvent('zeroclaw-session-change', { detail: { sessionId } }));
  };

  const handleDelete = async (e: React.MouseEvent, sessionId: string) => {
    e.stopPropagation();
    try {
      await deleteSession(sessionId);
      setSessions((prev) => prev.filter((s) => s.session_id !== sessionId));
      // If the deleted session was active, create a new one
      if (sessionId === currentSessionId) {
        handleNewChat();
      }
    } catch {
      // Ignore — server may not support delete yet
    }
  };

  const handleDoubleClick = (session: Session) => {
    setEditingId(session.session_id);
    setEditValue(session.name ?? '');
  };

  const commitRename = async () => {
    if (!editingId) return;
    const trimmed = editValue.trim();
    if (trimmed) {
      try {
        await renameSession(editingId, trimmed);
        setSessions((prev) =>
          prev.map((s) => (s.session_id === editingId ? { ...s, name: trimmed } : s)),
        );
      } catch {
        // Ignore — server may not support rename yet
      }
    }
    setEditingId(null);
    setEditValue('');
  };

  const handleRenameKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitRename();
    } else if (e.key === 'Escape') {
      setEditingId(null);
      setEditValue('');
    }
  };

  /** Derive a display label for a session. */
  const sessionLabel = (session: Session): string => {
    if (session.name) return session.name;
    // Use session_id prefix as fallback
    return `Session ${session.session_id.slice(0, 8)}`;
  };

  return (
    <div className="flex flex-col gap-1 mt-2">
      {/* New Chat button */}
      <button
        type="button"
        onClick={handleNewChat}
        className="flex items-center gap-2 rounded-xl px-3 py-2 text-sm font-medium transition-all w-full"
        style={{
          background: 'var(--pc-accent-glow)',
          border: '1px solid var(--pc-accent-dim)',
          color: 'var(--pc-accent-light)',
        }}
      >
        <Plus className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
        <span>New Chat</span>
      </button>

      {/* Session list */}
      <div className="flex flex-col gap-0.5 mt-1 overflow-y-auto max-h-[40vh]">
        {loading && (
          <p className="text-xs px-3 py-2" style={{ color: 'var(--pc-text-muted)' }}>
            Loading sessions...
          </p>
        )}

        {!loading && sessions.length === 0 && (
          <p className="text-xs px-3 py-2" style={{ color: 'var(--pc-text-muted)' }}>
            No sessions yet
          </p>
        )}

        {sessions.map((session) => {
          const isActive = session.session_id === currentSessionId;
          return (
            <div
              key={session.session_id}
              role="button"
              tabIndex={0}
              onClick={() => handleSelectSession(session.session_id)}
              onDoubleClick={() => handleDoubleClick(session)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleSelectSession(session.session_id);
              }}
              className="group flex items-center gap-2 rounded-xl px-3 py-2 text-sm cursor-pointer transition-all"
              style={{
                background: isActive ? 'var(--pc-accent-glow)' : 'transparent',
                border: isActive ? '1px solid var(--pc-accent-dim)' : '1px solid transparent',
                color: isActive ? 'var(--pc-accent-light)' : 'var(--pc-text-muted)',
              }}
              onMouseEnter={(e) => {
                if (!isActive) {
                  e.currentTarget.style.background = 'var(--pc-hover)';
                  e.currentTarget.style.color = 'var(--pc-text-secondary)';
                }
              }}
              onMouseLeave={(e) => {
                if (!isActive) {
                  e.currentTarget.style.background = 'transparent';
                  e.currentTarget.style.color = 'var(--pc-text-muted)';
                }
              }}
            >
              <MessageSquare
                className="h-4 w-4 shrink-0"
                style={{ color: isActive ? 'var(--pc-accent)' : undefined }}
              />
              <div className="flex-1 min-w-0">
                {editingId === session.session_id ? (
                  <input
                    ref={editInputRef}
                    type="text"
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    onBlur={commitRename}
                    onKeyDown={handleRenameKeyDown}
                    className="w-full bg-transparent text-sm outline-none"
                    style={{
                      color: 'var(--pc-text-primary)',
                      borderBottom: '1px solid var(--pc-accent)',
                    }}
                    onClick={(e) => e.stopPropagation()}
                  />
                ) : (
                  <>
                    <p className="truncate text-xs font-medium leading-tight">
                      {truncate(sessionLabel(session), 28)}
                    </p>
                    <p className="text-[10px] leading-tight mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>
                      {session.message_count} msgs &middot; {relativeTime(session.last_activity)}
                    </p>
                  </>
                )}
              </div>
              {/* Delete button — visible on hover */}
              {editingId !== session.session_id && (
                <button
                  type="button"
                  onClick={(e) => handleDelete(e, session.session_id)}
                  className="opacity-0 group-hover:opacity-100 transition-opacity p-1 rounded-lg shrink-0"
                  style={{ color: 'var(--pc-text-muted)' }}
                  onMouseEnter={(e) => { e.currentTarget.style.color = '#f87171'; }}
                  onMouseLeave={(e) => { e.currentTarget.style.color = 'var(--pc-text-muted)'; }}
                  aria-label="Delete session"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
