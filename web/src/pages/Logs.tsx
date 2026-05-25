import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Activity, ChevronDown, ChevronUp, Pause, Play, Plus, RefreshCw, X } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import type { LogEvent, LogsQueryParams, LogsResponse } from '@/lib/api';

const DEFAULT_SEVERITY_MIN = 9;
const PAGE_LIMIT = 200;
const POLL_INTERVAL_MS = 3000;
const RING_CAPACITY = 2000;

const SEVERITY_OPTIONS: { label: string; value: number | '' }[] = [
  { label: 'TRACE+', value: 1 },
  { label: 'DEBUG+', value: 5 },
  { label: 'INFO+', value: 9 },
  { label: 'WARN+', value: 13 },
  { label: 'ERROR+', value: 17 },
  { label: 'Any', value: '' },
];

const CATEGORY_OPTIONS = [
  '',
  'agent',
  'channel',
  'cron',
  'memory',
  'tool',
  'provider',
  'session',
  'system',
  'internal',
];

const OUTCOME_OPTIONS = ['', 'success', 'failure', 'unknown'];

interface FilterState {
  q: string;
  severityMin: number | '';
  category: string;
  outcome: string;
  action: string;
  hideInternal: boolean;
  sinceDaemonStart: boolean;
  fieldEq: Record<string, string>;
}

const DEFAULT_FILTER: FilterState = {
  q: '',
  severityMin: DEFAULT_SEVERITY_MIN,
  category: '',
  outcome: '',
  action: '',
  hideInternal: true,
  sinceDaemonStart: true,
  fieldEq: {},
};

function severityColor(severityNumber: number): { fg: string; bg: string; border: string } {
  if (severityNumber >= 17) {
    return {
      fg: 'var(--color-status-error)',
      bg: 'var(--color-status-error-alpha-08)',
      border: 'var(--color-status-error-alpha-20)',
    };
  }
  if (severityNumber >= 13) {
    return {
      fg: 'var(--color-status-warning)',
      bg: 'var(--color-status-warning-alpha-05)',
      border: 'var(--color-status-warning-alpha-20)',
    };
  }
  if (severityNumber >= 9) {
    return {
      fg: 'var(--color-status-info)',
      bg: 'color-mix(in srgb, var(--color-status-info) 6%, transparent)',
      border: 'color-mix(in srgb, var(--color-status-info) 20%, transparent)',
    };
  }
  return {
    fg: 'var(--pc-text-muted)',
    bg: 'var(--pc-hover)',
    border: 'var(--pc-border)',
  };
}

function formatTimestamp(raw: string): string {
  try {
    return new Date(raw).toLocaleTimeString(undefined, { hour12: false });
  } catch {
    return raw;
  }
}

function buildQueryParams(
  filter: FilterState,
  options: { sinceTs?: string; untilTs?: string; untilId?: string } = {},
): LogsQueryParams {
  const params: LogsQueryParams = {
    limit: PAGE_LIMIT,
    hide_internal: filter.hideInternal,
  };
  if (filter.q.trim()) params.q = filter.q.trim();
  if (filter.severityMin !== '') params.severity_min = filter.severityMin;
  if (filter.category) params.category = filter.category;
  if (filter.outcome) params.outcome = filter.outcome;
  if (filter.action.trim()) params.action = filter.action.trim();
  if (options.sinceTs) params.since_ts = options.sinceTs;
  if (options.untilTs) params.until_ts = options.untilTs;
  if (options.untilId) params.until_id = options.untilId;
  const fieldEq: Record<string, string> = {};
  for (const [key, value] of Object.entries(filter.fieldEq)) {
    if (value.trim()) fieldEq[key] = value.trim();
  }
  if (Object.keys(fieldEq).length > 0) params.field_eq = fieldEq;
  return params;
}

function fetchLogs(params: LogsQueryParams): Promise<LogsResponse> {
  const usp = new URLSearchParams();
  const { field_eq, ...rest } = params;
  for (const [key, value] of Object.entries(rest)) {
    if (value === undefined || value === null || value === '') continue;
    usp.set(key, String(value));
  }
  if (field_eq) {
    for (const [key, value] of Object.entries(field_eq)) {
      if (value === undefined || value === null || value === '') continue;
      usp.set(key, value);
    }
  }
  const qs = usp.toString();
  return apiFetch<LogsResponse>(`/api/logs${qs ? `?${qs}` : ''}`);
}

export default function Logs() {
  const [filter, setFilter] = useState<FilterState>(DEFAULT_FILTER);
  const [events, setEvents] = useState<LogEvent[]>([]);
  const [daemonStartedAt, setDaemonStartedAt] = useState('');
  const [attributionKeys, setAttributionKeys] = useState<string[]>([]);
  const [cursorOlder, setCursorOlder] = useState<[string, string] | null>(null);
  const [atEnd, setAtEnd] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [paused, setPaused] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [addingField, setAddingField] = useState(false);

  // Ring-buffer dedupe by id. Kept in a ref so the poll loop can read the
  // current state without re-binding via deps every tick.
  const eventsRef = useRef<LogEvent[]>([]);
  eventsRef.current = events;
  const filterRef = useRef(filter);
  filterRef.current = filter;
  const daemonStartedAtRef = useRef(daemonStartedAt);
  daemonStartedAtRef.current = daemonStartedAt;
  const pausedRef = useRef(paused);
  pausedRef.current = paused;

  const mergeNewer = useCallback((incoming: LogEvent[]) => {
    if (incoming.length === 0) return;
    setEvents((prev) => {
      const byId = new Map<string, LogEvent>();
      // incoming arrives newest-first per API contract
      for (const event of incoming) byId.set(event.id, event);
      for (const event of prev) if (!byId.has(event.id)) byId.set(event.id, event);
      const merged = Array.from(byId.values());
      merged.sort((left, right) =>
        right['@timestamp'].localeCompare(left['@timestamp']),
      );
      return merged.slice(0, RING_CAPACITY);
    });
  }, []);

  const initialLoad = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const sinceTs = filterRef.current.sinceDaemonStart
        ? daemonStartedAtRef.current || undefined
        : undefined;
      const response = await fetchLogs(buildQueryParams(filterRef.current, { sinceTs }));
      setEvents(response.events);
      setCursorOlder(response.next_cursor);
      setAtEnd(response.at_end);
      setAttributionKeys(response.attribution_keys ?? []);
      setDaemonStartedAt(response.daemon_started_at);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void initialLoad();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // One incremental fetch — fetch newer-than-newest, append. Exposed via
  // a ref so the Pause/Resume button can fire it inline on Resume to
  // close the gap immediately instead of waiting up to POLL_INTERVAL_MS
  // for the next scheduled tick.
  const tickRef = useRef<() => Promise<void>>(async () => {});
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      if (cancelled || pausedRef.current) return;
      const newest = eventsRef.current[0];
      const sinceTs = newest
        ? newest['@timestamp']
        : daemonStartedAtRef.current || undefined;
      try {
        const response = await fetchLogs(
          buildQueryParams(filterRef.current, { sinceTs }),
        );
        if (cancelled) return;
        if (response.events.length > 0) mergeNewer(response.events);
        if (response.daemon_started_at) setDaemonStartedAt(response.daemon_started_at);
        if (response.attribution_keys?.length) setAttributionKeys(response.attribution_keys);
      } catch {
        // Polling errors are silent — they'd cascade otherwise. Manual
        // Refresh surfaces errors prominently.
      }
    };
    tickRef.current = tick;
    const handle = window.setInterval(() => void tick(), POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(handle);
    };
  }, [mergeNewer]);

  const loadOlder = useCallback(async () => {
    if (!cursorOlder || atEnd || loadingOlder) return;
    setLoadingOlder(true);
    setError(null);
    try {
      const response = await fetchLogs(
        buildQueryParams(filterRef.current, {
          untilTs: cursorOlder[0],
          untilId: cursorOlder[1],
        }),
      );
      setEvents((prev) => {
        const byId = new Map<string, LogEvent>();
        for (const event of prev) byId.set(event.id, event);
        for (const event of response.events) if (!byId.has(event.id)) byId.set(event.id, event);
        const merged = Array.from(byId.values());
        merged.sort((left, right) =>
          right['@timestamp'].localeCompare(left['@timestamp']),
        );
        return merged.slice(0, RING_CAPACITY);
      });
      setCursorOlder(response.next_cursor);
      setAtEnd(response.at_end);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingOlder(false);
    }
  }, [atEnd, cursorOlder, loadingOlder]);

  // Filter changes invalidate the ring — re-base from the new constraints.
  const filterKey = useMemo(() => JSON.stringify(filter), [filter]);
  const skipFirstFilterRefetch = useRef(true);
  useEffect(() => {
    if (skipFirstFilterRefetch.current) {
      skipFirstFilterRefetch.current = false;
      return;
    }
    const timer = window.setTimeout(() => void initialLoad(), 200);
    return () => window.clearTimeout(timer);
  }, [filterKey, initialLoad]);

  const setFieldEq = (key: string, value: string) => {
    setFilter((prev) => {
      const next = { ...prev.fieldEq };
      if (value) next[key] = value;
      else delete next[key];
      return { ...prev, fieldEq: next };
    });
  };

  const activeFieldKeys = Object.entries(filter.fieldEq)
    .filter(([, value]) => value !== '')
    .map(([key]) => key);

  const inactiveAttributionKeys = attributionKeys.filter(
    (key) => !(key in filter.fieldEq),
  );

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center justify-between px-6 py-3 border-b"
        style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
      >
        <div className="flex items-center gap-3">
          <Activity className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Logs
          </h2>
          <span
            className="text-[10px] font-mono ml-2"
            style={{ color: 'var(--pc-text-faint)' }}
          >
            {events.length} events {atEnd ? '(end)' : ''}
          </span>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => {
              setPaused((value) => {
                const next = !value;
                // On resume, fire one immediate fetch with `since_ts =
                // newest known` so the gap between pause and resume
                // closes right away instead of waiting up to
                // POLL_INTERVAL_MS for the next scheduled tick. The
                // tick reads `pausedRef`, which is updated by React
                // after this setState commits — so defer the call to
                // the next microtask.
                if (!next) {
                  pausedRef.current = false;
                  void Promise.resolve().then(() => tickRef.current());
                }
                return next;
              });
            }}
            className="btn-electric flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold"
            style={{
              background: paused
                ? 'var(--color-status-warning)'
                : 'var(--color-status-success)',
              color: 'white',
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
          <button
            type="button"
            onClick={() => void initialLoad()}
            disabled={loading}
            className="btn-electric flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold"
          >
            <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
            Refresh
          </button>
        </div>
      </div>

      <div
        className="flex flex-wrap items-center gap-3 px-6 py-3 border-b"
        style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-base)' }}
      >
        <input
          type="search"
          value={filter.q}
          onChange={(event) => setFilter((prev) => ({ ...prev, q: event.target.value }))}
          placeholder="Search message + attributes"
          className="px-2 py-1 text-xs rounded border min-w-[220px] flex-1"
          style={{
            background: 'var(--pc-bg-surface)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-primary)',
          }}
        />
        <select
          value={filter.severityMin}
          onChange={(event) =>
            setFilter((prev) => ({
              ...prev,
              severityMin:
                event.target.value === '' ? '' : Number.parseInt(event.target.value, 10),
            }))
          }
          className="px-2 py-1 text-xs rounded border"
          style={{
            background: 'var(--pc-bg-surface)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-primary)',
          }}
        >
          {SEVERITY_OPTIONS.map((option) => (
            <option key={String(option.value)} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
        <select
          value={filter.category}
          onChange={(event) => setFilter((prev) => ({ ...prev, category: event.target.value }))}
          className="px-2 py-1 text-xs rounded border"
          style={{
            background: 'var(--pc-bg-surface)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-primary)',
          }}
        >
          {CATEGORY_OPTIONS.map((option) => (
            <option key={option} value={option}>
              {option || 'Any category'}
            </option>
          ))}
        </select>
        <select
          value={filter.outcome}
          onChange={(event) => setFilter((prev) => ({ ...prev, outcome: event.target.value }))}
          className="px-2 py-1 text-xs rounded border"
          style={{
            background: 'var(--pc-bg-surface)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-primary)',
          }}
        >
          {OUTCOME_OPTIONS.map((option) => (
            <option key={option} value={option}>
              {option || 'Any outcome'}
            </option>
          ))}
        </select>
        <input
          type="text"
          value={filter.action}
          onChange={(event) => setFilter((prev) => ({ ...prev, action: event.target.value }))}
          placeholder="event.action"
          className="px-2 py-1 text-xs rounded border w-[160px]"
          style={{
            background: 'var(--pc-bg-surface)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-primary)',
          }}
        />
        <label
          className="flex items-center gap-1.5 text-[11px] cursor-pointer"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          <input
            type="checkbox"
            checked={filter.hideInternal}
            onChange={(event) =>
              setFilter((prev) => ({ ...prev, hideInternal: event.target.checked }))
            }
            style={{ accentColor: 'var(--pc-accent)' }}
          />
          Hide internal
        </label>
        <label
          className="flex items-center gap-1.5 text-[11px] cursor-pointer"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          <input
            type="checkbox"
            checked={filter.sinceDaemonStart}
            onChange={(event) =>
              setFilter((prev) => ({ ...prev, sinceDaemonStart: event.target.checked }))
            }
            style={{ accentColor: 'var(--pc-accent)' }}
          />
          Since daemon start
        </label>
        <button
          type="button"
          onClick={() => setFiltersOpen((value) => !value)}
          className="flex items-center gap-1 text-[11px] px-2 py-1 rounded border"
          style={{
            background: 'var(--pc-bg-surface)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-muted)',
          }}
        >
          {filtersOpen ? (
            <ChevronUp className="h-3 w-3" />
          ) : (
            <ChevronDown className="h-3 w-3" />
          )}
          zeroclaw.* {activeFieldKeys.length > 0 && `(${activeFieldKeys.length})`}
        </button>
      </div>

      {filtersOpen && (
        <div
          className="flex flex-wrap items-center gap-2 px-6 py-2 border-b"
          style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
        >
          {activeFieldKeys.map((key) => (
            <span
              key={key}
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-[10px] font-mono"
              style={{
                background: 'var(--pc-bg-base)',
                borderColor: 'var(--pc-border)',
                color: 'var(--pc-text-primary)',
              }}
            >
              <span style={{ color: 'var(--pc-text-faint)' }}>{key}=</span>
              <input
                type="text"
                value={filter.fieldEq[key] ?? ''}
                onChange={(event) => setFieldEq(key, event.target.value)}
                className="bg-transparent outline-none w-[100px] text-[10px] font-mono"
                style={{ color: 'var(--pc-text-primary)' }}
              />
              <button
                type="button"
                onClick={() => setFieldEq(key, '')}
                style={{ color: 'var(--pc-text-faint)' }}
                aria-label={`Remove ${key} filter`}
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}
          {addingField ? (
            <select
              autoFocus
              onChange={(event) => {
                const key = event.target.value;
                if (key) setFieldEq(key, '');
                setAddingField(false);
              }}
              onBlur={() => setAddingField(false)}
              defaultValue=""
              className="px-2 py-1 text-[10px] rounded border"
              style={{
                background: 'var(--pc-bg-base)',
                borderColor: 'var(--pc-border)',
                color: 'var(--pc-text-primary)',
              }}
            >
              <option value="" disabled>
                Pick a key…
              </option>
              {inactiveAttributionKeys.map((key) => (
                <option key={key} value={key}>
                  {key}
                </option>
              ))}
            </select>
          ) : (
            <button
              type="button"
              onClick={() => setAddingField(true)}
              disabled={inactiveAttributionKeys.length === 0}
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded border text-[10px]"
              style={{
                background: 'var(--pc-bg-base)',
                borderColor: 'var(--pc-border)',
                color: 'var(--pc-text-muted)',
              }}
            >
              <Plus className="h-3 w-3" /> Add filter
            </button>
          )}
          {activeFieldKeys.length > 0 && (
            <button
              type="button"
              onClick={() => setFilter((prev) => ({ ...prev, fieldEq: {} }))}
              className="text-[10px] ml-1"
              style={{ color: 'var(--pc-accent)' }}
            >
              clear
            </button>
          )}
        </div>
      )}

      {error && (
        <div
          className="px-6 py-2 text-xs border-b"
          style={{
            color: 'var(--color-status-error)',
            background: 'var(--color-status-error-alpha-08)',
            borderColor: 'var(--color-status-error-alpha-20)',
          }}
        >
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-4 space-y-1 min-h-0">
        {events.length === 0 && !loading ? (
          <div
            className="flex flex-col items-center justify-center h-full"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            <Activity
              className="h-10 w-10 mb-3"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <p className="text-sm">No events match the current filters.</p>
          </div>
        ) : (
          events.map((event) => <LogRow key={event.id} event={event} />)
        )}
        {!atEnd && events.length > 0 && (
          <div className="flex justify-center pt-3">
            <button
              type="button"
              onClick={() => void loadOlder()}
              disabled={loadingOlder || !cursorOlder}
              className="btn-electric px-3 py-1.5 text-xs font-semibold"
            >
              {loadingOlder ? 'Loading…' : 'Load older'}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function LogRow({ event }: { event: LogEvent }) {
  const style = severityColor(event.severity_number);
  const attribution = event.zeroclaw ?? {};
  const attributionEntries = Object.entries(attribution).filter(
    ([key, value]) => key !== 'duration_ms' && value !== '' && value !== null,
  );
  const hasMessage = typeof event.message === 'string' && event.message.length > 0;
  return (
    <div
      className="rounded-md px-3 py-2 border text-xs"
      style={{ borderColor: style.border, background: style.bg }}
    >
      <div className="flex items-start gap-3">
        <span
          className="font-mono whitespace-nowrap mt-0.5 text-[10px]"
          style={{ color: 'var(--pc-text-faint)' }}
        >
          {formatTimestamp(event['@timestamp'])}
        </span>
        <span
          className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-semibold border flex-shrink-0"
          style={{ color: style.fg, borderColor: style.border, background: 'transparent' }}
        >
          {event.severity_text}
        </span>
        <span
          className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-mono border flex-shrink-0"
          style={{
            color: 'var(--pc-text-muted)',
            borderColor: 'var(--pc-border)',
            background: 'var(--pc-bg-base)',
          }}
        >
          {event.event.category}.{event.event.action}
        </span>
        <div className="flex-1 min-w-0">
          {hasMessage && (
            <p
              className="text-sm break-words"
              style={{ color: 'var(--pc-text-primary)' }}
            >
              {event.message}
            </p>
          )}
          {attributionEntries.length > 0 && (
            <div
              className={`${hasMessage ? 'mt-1' : ''} flex flex-wrap gap-x-3 gap-y-0.5 text-[10px] font-mono`}
              style={{ color: 'var(--pc-text-muted)' }}
            >
              {attributionEntries.map(([key, value]) => (
                <span key={key}>
                  <span style={{ color: 'var(--pc-text-faint)' }}>{key}=</span>
                  {String(value)}
                </span>
              ))}
              {typeof attribution.duration_ms === 'number' && (
                <span>
                  <span style={{ color: 'var(--pc-text-faint)' }}>duration_ms=</span>
                  {attribution.duration_ms}
                </span>
              )}
            </div>
          )}
          {event.attributes && Object.keys(event.attributes).length > 0 && (
            <details className="mt-1">
              <summary
                className="cursor-pointer text-[10px]"
                style={{ color: 'var(--pc-text-faint)' }}
              >
                attributes ({Object.keys(event.attributes).length})
              </summary>
              <pre
                className="mt-1 p-2 rounded text-[10px] overflow-x-auto"
                style={{
                  background: 'var(--pc-bg-base)',
                  color: 'var(--pc-text-muted)',
                  borderColor: 'var(--pc-border)',
                }}
              >
                {JSON.stringify(event.attributes, null, 2)}
              </pre>
            </details>
          )}
        </div>
      </div>
    </div>
  );
}
