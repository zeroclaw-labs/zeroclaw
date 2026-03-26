// web/src/components/SessionSidebar.tsx

import { useState } from 'react';
import { Plus, Trash2, MessageSquare, Search } from 'lucide-react';
import type { Session } from '@/types/session';

interface SessionSidebarProps {
  sessions: Session[];
  activeSessionId: string | null;
  onSelectSession: (id: string) => void;
  onNewChat: () => void;
  onDeleteSession: (id: string) => void;
}

/** 按时间分组 */
function groupByDate(sessions: Session[]): { label: string; items: Session[] }[] {
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const yesterdayStart = todayStart - 86_400_000;
  const weekStart = todayStart - 7 * 86_400_000;

  const today: Session[] = [];
  const yesterday: Session[] = [];
  const last7Days: Session[] = [];
  const older: Session[] = [];

  for (const s of sessions) {
    const t = new Date(s.updated_at).getTime();
    if (t >= todayStart) {
      today.push(s);
    } else if (t >= yesterdayStart) {
      yesterday.push(s);
    } else if (t >= weekStart) {
      last7Days.push(s);
    } else {
      older.push(s);
    }
  }

  const result: { label: string; items: Session[] }[] = [];
  if (today.length > 0) result.push({ label: 'Today', items: today });
  if (yesterday.length > 0) result.push({ label: 'Yesterday', items: yesterday });
  if (last7Days.length > 0) result.push({ label: 'Last 7 days', items: last7Days });
  if (older.length > 0) result.push({ label: 'Older', items: older });
  return result;
}

export default function SessionSidebar({
  sessions,
  activeSessionId,
  onSelectSession,
  onNewChat,
  onDeleteSession,
}: SessionSidebarProps) {
  const [search, setSearch] = useState('');

  const filtered = search
    ? sessions.filter((s) => s.title.toLowerCase().includes(search.toLowerCase()))
    : sessions;
  const groups = groupByDate(filtered);

  return (
    <div
      className="flex flex-col h-full w-[260px] flex-shrink-0"
      style={{
        background: 'var(--pc-bg-sidebar)',
        borderRight: '1px solid var(--pc-border)',
      }}
    >
      {/* New chat button */}
      <div className="p-3">
        <button
          onClick={onNewChat}
          className="w-full flex items-center justify-center gap-2 px-3 py-2.5 rounded-xl text-sm font-medium transition-all duration-300 border text-sm hover:text-white"
          style={{
            background: 'var(--pc-bg-elevated)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-secondary)',
          }}
          onMouseEnter={(e) => {
            e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
            e.currentTarget.style.background = 'var(--pc-accent-glow)';
          }}
          onMouseLeave={(e) => {
            e.currentTarget.style.borderColor = 'var(--pc-border)';
            e.currentTarget.style.background = 'var(--pc-bg-elevated)';
          }}
        >
          <Plus className="h-4 w-4" />
          New chat
        </button>
      </div>

      {/* Search */}
      <div className="px-3 pb-2">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5" style={{ color: 'var(--pc-text-muted)' }} />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search sessions..."
            className="w-full pl-8 pr-3 py-1.5 rounded-lg text-xs text-[#e8edf5] placeholder:text-[#556080] focus:outline-none transition-colors"
            style={{
              background: 'var(--pc-bg-input)',
              border: '1px solid var(--pc-border)',
              color: 'var(--pc-text-primary)',
            }}
            onFocus={(e) => {
              e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
            }}
            onBlur={(e) => {
              e.currentTarget.style.borderColor = 'var(--pc-border)';
            }}
          />
        </div>
      </div>

      {/* Session list */}
      <div className="flex-1 overflow-y-auto px-2 pb-3">
        {groups.length === 0 && (
          <div className="px-3 py-8 text-center">
            <MessageSquare className="h-8 w-8 mx-auto mb-2" style={{ color: 'var(--pc-border-strong)' }} />
            <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>No sessions yet</p>
          </div>
        )}

        {groups.map((group) => (
          <div key={group.label} className="mb-3">
            <p className="px-2 py-1.5 text-[10px] font-medium uppercase tracking-wider" style={{ color: 'var(--pc-text-muted)' }}>
              {group.label}
            </p>
            {group.items.map((session) => {
              const isActive = session.id === activeSessionId;
              return (
                <button
                  key={session.id}
                  onClick={() => onSelectSession(session.id)}
                  className="group w-full flex items-center gap-2 px-2.5 py-2 rounded-lg text-left text-sm transition-all duration-200 mb-0.5 border-l-2"
                  style={{
                    color: isActive ? 'var(--pc-text-primary)' : 'var(--pc-text-secondary)',
                    borderColor: isActive ? 'var(--pc-accent)' : 'transparent',
                    background: isActive ? 'var(--pc-accent-glow)' : 'transparent',
                  }}
                  onMouseEnter={(e) => {
                    if (!isActive) {
                      e.currentTarget.style.background = 'var(--pc-hover)';
                      e.currentTarget.style.color = 'var(--pc-text-primary)';
                    }
                  }}
                  onMouseLeave={(e) => {
                    if (!isActive) {
                      e.currentTarget.style.background = 'transparent';
                      e.currentTarget.style.color = 'var(--pc-text-secondary)';
                    }
                  }}
                >
                  <MessageSquare className="h-3.5 w-3.5 flex-shrink-0" style={{ color: isActive ? 'var(--pc-accent)' : 'var(--pc-text-muted)', opacity: 0.5 }} />
                  <span className="flex-1 truncate text-xs">{session.title}</span>
                  <span
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeleteSession(session.id);
                    }}
                    className="opacity-0 group-hover:opacity-100 p-1 rounded transition-all cursor-pointer"
                    style={{ color: 'var(--pc-text-muted)' }}
                    onMouseEnter={(e) => {
                      e.currentTarget.style.color = 'var(--pc-accent-light)';
                      e.currentTarget.style.background = 'rgba(255, 68, 102, 0.1)';
                    }}
                    onMouseLeave={(e) => {
                      e.currentTarget.style.color = 'var(--pc-text-muted)';
                      e.currentTarget.style.background = 'transparent';
                    }}
                  >
                    <Trash2 className="h-3 w-3" />
                  </span>
                </button>
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
