import { useState, useEffect, useRef, useCallback } from 'react';
import {
  Activity,
  Pause,
  Play,
  ArrowDown,
  Filter,
} from 'lucide-react';
import type { SSEEvent } from '@/types/api';
import { SSEClient } from '@/lib/sse';

function formatTimestamp(ts?: string): string {
  if (!ts) return new Date().toLocaleTimeString();
  return new Date(ts).toLocaleTimeString();
}

function eventTypeBadgeColor(type: string): { color: string; bg: string; border: string } {
  switch (type.toLowerCase()) {
    case 'error':
      return { color: 'var(--color-status-error)', bg: 'var(--color-status-error)', border: 'var(--color-status-error)' };
    case 'warn':
    case 'warning':
      return { color: 'var(--color-status-warning)', bg: 'var(--color-status-warning)', border: 'var(--color-status-warning)' };
    case 'tool_call':
    case 'tool_result':
      return { color: '#a855f7', bg: '#a855f7', border: '#a855f7' };
    case 'message':
    case 'chat':
      return { color: 'var(--color-accent-blue)', bg: 'var(--color-accent-blue)', border: 'var(--color-accent-blue)' };
    case 'health':
    case 'status':
      return { color: 'var(--color-status-success)', bg: 'var(--color-status-success)', border: 'var(--color-status-success)' };
    default:
      return { color: 'var(--color-text-muted)', bg: 'var(--color-text-muted)', border: 'var(--color-border-default)' };
  }
}

interface LogEntry {
  id: string;
  event: SSEEvent;
}

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

  useEffect(() => {
    pausedRef.current = paused;
  }, [paused]);

  useEffect(() => {
    const client = new SSEClient();

    client.onConnect = () => {
      setConnected(true);
    };

    client.onError = () => {
      setConnected(false);
    };

    client.onEvent = (event: SSEEvent) => {
      if (pausedRef.current) return;
      entryIdRef.current += 1;
      const entry: LogEntry = {
        id: `log-${entryIdRef.current}`,
        event,
      };
      setEntries((prev) => {
        const next = [...prev, entry];
        return next.length > 500 ? next.slice(-500) : next;
      });
    };

    client.connect();
    sseRef.current = client;

    return () => {
      client.disconnect();
    };
  }, []);

  useEffect(() => {
    if (autoScroll && containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [entries, autoScroll]);

  const handleScroll = useCallback(() => {
    if (!containerRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
    const isAtBottom = scrollHeight - scrollTop - clientHeight < 50;
    setAutoScroll(isAtBottom);
  }, []);

  const jumpToBottom = () => {
    if (containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
    setAutoScroll(true);
  };

  const allTypes = Array.from(new Set(entries.map((e) => e.event.type))).sort();

  const toggleTypeFilter = (type: string) => {
    setTypeFilters((prev) => {
      const next = new Set(prev);
      if (next.has(type)) {
        next.delete(type);
      } else {
        next.add(type);
      }
      return next;
    });
  };

  const filteredEntries =
    typeFilters.size === 0
      ? entries
      : entries.filter((e) => typeFilters.has(e.event.type));

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      <div className="flex items-center justify-between px-6 py-3 border-b animate-fade-in" style={{ borderColor: 'var(--color-border-default)', backgroundColor: 'var(--color-bg-header)' }}>
        <div className="flex items-center gap-3">
          <Activity className="h-5 w-5" style={{ color: 'var(--color-accent-blue)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-primary)' }}>Live Logs</h2>
          <div className="flex items-center gap-2 ml-2">
            <span
              className="inline-block h-1.5 w-1.5 rounded-full"
              style={{ backgroundColor: connected ? 'var(--color-status-success)' : 'var(--color-status-error)' }}
            />
            <span className="text-xs" style={{ color: 'var(--color-text-muted)' }}>
              {connected ? 'Connected' : 'Disconnected'}
            </span>
          </div>
          <span className="text-xs ml-2 font-mono" style={{ color: 'var(--color-text-muted)' }}>
            {filteredEntries.length} events
          </span>
        </div>

        <div className="flex items-center gap-2">
          <button
            onClick={() => setPaused(!paused)}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-xl text-xs font-semibold transition-all duration-300"
            style={{
              background: paused
                ? 'linear-gradient(135deg, var(--color-status-success), var(--color-status-success-hover, #00cc7a))'
                : 'linear-gradient(135deg, var(--color-status-warning), #ee9900)',
              color: 'white'
            }}
          >
            {paused ? (
              <>
                <Play className="h-3.5 w-3.5" /> Resume
              </>
            ) : (
              <>
                <Pause className="h-3.5 w-3.5" /> Pause
              </>
            )}
          </button>

          {!autoScroll && (
            <button
              onClick={jumpToBottom}
              className="btn-electric flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold"
            >
              <ArrowDown className="h-3.5 w-3.5" />
              Jump to bottom
            </button>
          )}
        </div>
      </div>

      {allTypes.length > 0 && (
        <div className="flex items-center gap-2 px-6 py-2 border-b overflow-x-auto" style={{ borderColor: 'var(--color-border-subtle)', backgroundColor: 'var(--color-bg-primary)', opacity: 0.6 }}>
          <Filter className="h-3.5 w-3.5 flex-shrink-0" style={{ color: 'var(--color-text-muted)' }} />
          <span className="text-xs flex-shrink-0 uppercase tracking-wider" style={{ color: 'var(--color-text-muted)' }}>Filter:</span>
          {allTypes.map((type) => (
            <label
              key={type}
              className="flex items-center gap-1.5 cursor-pointer flex-shrink-0"
            >
              <input
                type="checkbox"
                checked={typeFilters.has(type)}
                onChange={() => toggleTypeFilter(type)}
                className="rounded h-3 w-3"
                style={{ 
                  backgroundColor: 'var(--color-bg-input)', 
                  borderColor: 'var(--color-border-default)', 
                  accentColor: 'var(--color-accent-blue)' 
                }}
              />
              <span className="text-xs capitalize" style={{ color: 'var(--color-text-muted)' }}>{type}</span>
            </label>
          ))}
          {typeFilters.size > 0 && (
            <button
              onClick={() => setTypeFilters(new Set())}
              className="text-xs flex-shrink-0 ml-1 transition-colors"
              style={{ color: 'var(--color-accent-blue)' }}
            >
              Clear
            </button>
          )}
        </div>
      )}

      <div
        ref={containerRef}
        onScroll={handleScroll}
        className="flex-1 overflow-y-auto p-4 space-y-1.5"
      >
        {filteredEntries.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full animate-fade-in" style={{ color: 'var(--color-text-muted)' }}>
            <Activity className="h-10 w-10 mb-3" style={{ color: 'var(--color-border-default)' }} />
            <p className="text-sm">
              {paused
                ? 'Log streaming is paused.'
                : 'Waiting for events...'}
            </p>
          </div>
        ) : (
          filteredEntries.map((entry) => {
            const { event } = entry;
            const badge = eventTypeBadgeColor(event.type);
            const detail =
              event.message ??
              event.content ??
              event.data ??
              JSON.stringify(
                Object.fromEntries(
                  Object.entries(event).filter(
                    ([k]) => k !== 'type' && k !== 'timestamp',
                  ),
                ),
              );

            return (
              <div
                key={entry.id}
                className="glass-card rounded-lg p-3 hover:border-[var(--color-accent-blue)] transition-all duration-200"
              >
                <div className="flex items-start gap-3">
                  <span className="text-xs font-mono whitespace-nowrap mt-0.5" style={{ color: 'var(--color-text-muted)' }}>
                    {formatTimestamp(event.timestamp)}
                  </span>
                  <span
                    className="inline-flex items-center px-2 py-0.5 rounded text-xs font-semibold border capitalize flex-shrink-0"
                    style={{ 
                      color: badge.color, 
                      backgroundColor: badge.bg, 
                      opacity: 0.15,
                      borderColor: badge.border
                    }}
                  >
                    {event.type}
                  </span>
                  <p className="text-sm break-all min-w-0" style={{ color: 'var(--color-text-secondary)' }}>
                    {typeof detail === 'string' ? detail : JSON.stringify(detail)}
                  </p>
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
