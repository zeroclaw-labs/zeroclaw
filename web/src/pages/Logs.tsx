import { useState, useEffect, useRef, useCallback } from 'react';
import { Activity, Pause, Play, ArrowDown, Filter } from 'lucide-react';
import type { SSEEvent } from '@/types/api';
import { SSEClient } from '@/lib/sse';
import { t } from '@/lib/i18n';

function formatTimestamp(ts?: string): string {
  return ts ? new Date(ts).toLocaleTimeString() : new Date().toLocaleTimeString();
}

function eventTypeStyle(type: string): { color: string; bg: string; border: string } {
  switch (type.toLowerCase()) {
    case 'error': return { color: 'var(--color-status-error)', bg: 'rgba(239, 68, 68, 0.06)', border: 'rgba(239, 68, 68, 0.2)' };
    case 'warn': case 'warning': return { color: 'var(--color-status-warning)', bg: 'rgba(255, 170, 0, 0.06)', border: 'rgba(255, 170, 0, 0.2)' };
    case 'tool_call': case 'tool_result': return { color: '#a78bfa', bg: 'rgba(167, 139, 250, 0.06)', border: 'rgba(167, 139, 250, 0.2)' };
    case 'message': case 'chat': return { color: 'var(--pc-accent)', bg: 'var(--pc-accent-glow)', border: 'var(--pc-accent-dim)' };
    case 'health': case 'status': return { color: 'var(--color-status-success)', bg: 'rgba(0, 230, 138, 0.06)', border: 'rgba(0, 230, 138, 0.2)' };
    default: return { color: 'var(--pc-text-muted)', bg: 'var(--pc-hover)', border: 'var(--pc-border)' };
  }
}

interface LogEntry { id: string; event: SSEEvent; }

export default function Logs() {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [paused, setPaused] = useState(false);
  const [connected, setConnected] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const [typeFilters, setTypeFilters] = useState<Set<string>>(new Set());
  const containerRef = useRef<HTMLDivElement>(null);
  const sseRef = useRef<SSEClient | null>(null);
  const pausedRef = useRef(false);
  const entryIdRef = useRef(0);

  useEffect(() => { pausedRef.current = paused; }, [paused]);

  useEffect(() => {
    const client = new SSEClient();
    client.onConnect = () => setConnected(true);
    client.onError = () => setConnected(false);
    client.onEvent = (event: SSEEvent) => {
      if (pausedRef.current) return;
      entryIdRef.current += 1;
      setEntries((prev) => {
        const next = [...prev, { id: `log-${entryIdRef.current}`, event }];
        return next.length > 500 ? next.slice(-500) : next;
      });
    };
    client.connect();
    sseRef.current = client;
    return () => { client.disconnect(); };
  }, []);

  useEffect(() => {
    if (autoScroll && containerRef.current) containerRef.current.scrollTop = containerRef.current.scrollHeight;
  }, [entries, autoScroll]);

  const handleScroll = useCallback(() => {
    if (!containerRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
    setAutoScroll(scrollHeight - scrollTop - clientHeight < 50);
  }, []);

  const jumpToBottom = () => {
    if (containerRef.current) containerRef.current.scrollTop = containerRef.current.scrollHeight;
    setAutoScroll(true);
  };

  const allTypes = Array.from(new Set(entries.map((e) => e.event.type))).sort();

  const toggleTypeFilter = (type: string) => {
    setTypeFilters((prev) => {
      const next = new Set(prev);
      next.has(type) ? next.delete(type) : next.add(type);
      return next;
    });
  };

  const filteredEntries = typeFilters.size === 0 ? entries : entries.filter((e) => typeFilters.has(e.event.type));

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Toolbar */}
      <div className="flex items-center justify-between px-6 py-3 border-b animate-fade-in" style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}>
        <div className="flex items-center gap-3">
          <Activity className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>{t('logs.live_logs')}</h2>
          <div className="flex items-center gap-2 ml-2">
            <span className="status-dot" style={connected ? { background: 'var(--color-status-success)', boxShadow: '0 0 6px var(--color-status-success)' } : { background: 'var(--color-status-error)', boxShadow: '0 0 6px var(--color-status-error)' }} />
            <span className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>{connected ? t('logs.connected') : t('logs.disconnected')}</span>
          </div>
          <span className="text-[10px] font-mono ml-2" style={{ color: 'var(--pc-text-faint)' }}>{filteredEntries.length} {t('logs.events')}</span>
        </div>
        <div className="flex items-center gap-2">
          <button onClick={() => setPaused(!paused)} className="btn-electric flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold" style={{ background: paused ? 'var(--color-status-success)' : 'var(--color-status-warning)', color: 'white' }}>
            {paused ? <><Play className="h-3.5 w-3.5" />{t('logs.resume')}</> : <><Pause className="h-3.5 w-3.5" />{t('logs.pause')}</>}
          </button>
          {!autoScroll && (
            <button onClick={jumpToBottom} className="btn-electric flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold">
              <ArrowDown className="h-3.5 w-3.5" />{t('logs.jump_to_bottom')}
            </button>
          )}
        </div>
      </div>

      {/* Type filters */}
      {allTypes.length > 0 && (
        <div className="flex items-center gap-2 px-6 py-2 border-b overflow-x-auto" style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-base)' }}>
          <Filter className="h-3.5 w-3.5 flex-shrink-0" style={{ color: 'var(--pc-text-faint)' }} />
          <span className="text-[10px] uppercase tracking-wider flex-shrink-0" style={{ color: 'var(--pc-text-faint)' }}>{t('logs.filter_label')}:</span>
          {allTypes.map((type) => (
            <label key={type} className="flex items-center gap-1.5 cursor-pointer flex-shrink-0">
              <input type="checkbox" checked={typeFilters.has(type)} onChange={() => toggleTypeFilter(type)} className="rounded" style={{ accentColor: 'var(--pc-accent)' }} />
              <span className="text-[10px] capitalize" style={{ color: 'var(--pc-text-muted)' }}>{type}</span>
            </label>
          ))}
          {typeFilters.size > 0 && (
            <button onClick={() => setTypeFilters(new Set())} className="text-[10px] flex-shrink-0 ml-1 transition-colors" style={{ color: 'var(--pc-accent)' }}>{t('logs.clear')}</button>
          )}
        </div>
      )}

      {/* Log entries */}
      <div ref={containerRef} onScroll={handleScroll} className="flex-1 overflow-y-auto p-4 space-y-2">
        {filteredEntries.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full animate-fade-in" style={{ color: 'var(--pc-text-muted)' }}>
            <Activity className="h-10 w-10 mb-3" style={{ color: 'var(--pc-text-faint)' }} />
            <p className="text-sm">{paused ? t('logs.paused_hint') : t('logs.waiting_hint')}</p>
          </div>
        ) : filteredEntries.map((entry) => {
          const { event } = entry;
          const style = eventTypeStyle(event.type);
          const detail = event.message ?? event.content ?? event.data ?? JSON.stringify(Object.fromEntries(Object.entries(event).filter(([k]) => k !== 'type' && k !== 'timestamp')));
          return (
            <div key={entry.id} className="card rounded-xl p-3">
              <div className="flex items-start gap-3">
                <span className="text-[10px] font-mono whitespace-nowrap mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>{formatTimestamp(event.timestamp)}</span>
                <span className="inline-flex items-center px-2 py-0.5 rounded text-[10px] font-semibold border capitalize flex-shrink-0" style={style}>{event.type}</span>
                <p className="text-sm break-all min-w-0" style={{ color: 'var(--pc-text-secondary)' }}>{typeof detail === 'string' ? detail : JSON.stringify(detail)}</p>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
