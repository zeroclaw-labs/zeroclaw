import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Activity, ChevronDown, ChevronUp, Pause, Play, Plus, RefreshCw, X } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import type { LogEvent, LogsQueryParams, LogsResponse } from '@/lib/api';
import { Badge, Button, PageHeader } from '@/components/ui';

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

// Shared token classes for the tokenized filter controls — keeps the
// inputs/selects calm and consistent without repeating the long class list.
const CONTROL_CLASS =
  'px-2 py-1 text-xs rounded-[var(--radius-md)] border border-pc-border ' +
  'bg-pc-input text-pc-text placeholder:text-pc-text-faint ' +
  'focus-visible:outline-none focus-visible:border-pc-accent ' +
  'focus-visible:ring-1 focus-visible:ring-pc-accent';

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

// Level styling keyed off severity number, expressed as token classes
// (status-error / warning / info for the level, neutral muted for trace/debug).
function severityClasses(severityNumber: number): { text: string; chip: string } {
  if (severityNumber >= 17) {
    return {
      text: 'text-status-error',
      chip: 'text-status-error border-status-error/40 bg-status-error/10',
    };
  }
  if (severityNumber >= 13) {
    return {
      text: 'text-status-warning',
      chip: 'text-status-warning border-status-warning/40 bg-status-warning/10',
    };
  }
  if (severityNumber >= 9) {
    return {
      text: 'text-status-info',
      chip: 'text-status-info border-status-info/40 bg-status-info/10',
    };
  }
  return {
    text: 'text-pc-text-muted',
    chip: 'text-pc-text-muted border-pc-border bg-pc-elevated',
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
      // Skip while paused, cancelled, or the tab is hidden (no background poll).
      if (cancelled || pausedRef.current || document.hidden) return;
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
    // Catch up immediately when the tab becomes visible again.
    const onVisible = () => {
      if (!document.hidden) void tick();
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      cancelled = true;
      window.clearInterval(handle);
      document.removeEventListener('visibilitychange', onVisible);
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

  const setFieldEq = useCallback((key: string, value: string) => {
    setFilter((prev) => {
      const next = { ...prev.fieldEq };
      if (value) next[key] = value;
      else delete next[key];
      return { ...prev, fieldEq: next };
    });
  }, []);

  // Click-to-filter from a log row's attribution chips. The dedicated
  // event.action filter lives at the top level (`action`); every other
  // attribution key is an exact match in `field_eq`. Setting either re-bases
  // the stream via the existing filter-change effect — no special-casing here.
  const setActionFilter = useCallback((value: string) => {
    setFilter((prev) => ({ ...prev, action: value }));
  }, []);

  const activeFieldKeys = Object.entries(filter.fieldEq)
    .filter(([, value]) => value !== '')
    .map(([key]) => key);

  const inactiveAttributionKeys = attributionKeys.filter(
    (key) => !(key in filter.fieldEq),
  );

  return (
    <div className="flex flex-col h-full">
      <div className="px-6 py-4 border-b border-pc-border bg-pc-surface">
        <PageHeader
          title={
            <span className="flex items-center gap-2">
              <Activity className="h-5 w-5 text-pc-accent" />
              Logs
            </span>
          }
          actions={
            <>
              <Badge tone="neutral">
                {events.length} events{atEnd ? ' · end' : ''}
              </Badge>
              <Button
                variant="ghost"
                size="sm"
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
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => void initialLoad()}
                disabled={loading}
              >
                <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
                Refresh
              </Button>
            </>
          }
        />
      </div>

      <div className="flex flex-wrap items-center gap-3 px-6 py-3 border-b border-pc-border bg-pc-base">
        <input
          type="search"
          value={filter.q}
          onChange={(event) => setFilter((prev) => ({ ...prev, q: event.target.value }))}
          placeholder="Search message + attributes"
          className={`${CONTROL_CLASS} min-w-[220px] flex-1`}
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
          className={CONTROL_CLASS}
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
          className={CONTROL_CLASS}
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
          className={CONTROL_CLASS}
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
          className={`${CONTROL_CLASS} w-[160px]`}
        />
        <label className="flex items-center gap-1.5 text-[11px] cursor-pointer text-pc-text-muted">
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
        <label className="flex items-center gap-1.5 text-[11px] cursor-pointer text-pc-text-muted">
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
          className="flex items-center gap-1 text-[11px] px-2 py-1 rounded-[var(--radius-md)] border border-pc-border bg-pc-surface text-pc-text-muted transition-colors hover:bg-pc-elevated/60 hover:text-pc-text"
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
        <div className="flex flex-wrap items-center gap-2 px-6 py-2 border-b border-pc-border bg-pc-surface">
          {activeFieldKeys.map((key) => (
            <span
              key={key}
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded-[var(--radius-md)] border border-pc-border bg-pc-base text-[10px] font-mono text-pc-text"
            >
              <span className="text-pc-text-faint">{key}=</span>
              <input
                type="text"
                value={filter.fieldEq[key] ?? ''}
                onChange={(event) => setFieldEq(key, event.target.value)}
                className="bg-transparent outline-none w-[100px] text-[10px] font-mono text-pc-text"
              />
              <button
                type="button"
                onClick={() => setFieldEq(key, '')}
                className="text-pc-text-faint hover:text-pc-text transition-colors"
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
              className="px-2 py-1 text-[10px] rounded-[var(--radius-md)] border border-pc-border bg-pc-base text-pc-text"
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
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded-[var(--radius-md)] border border-pc-border bg-pc-base text-[10px] text-pc-text-muted transition-colors hover:text-pc-text disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <Plus className="h-3 w-3" /> Add filter
            </button>
          )}
          {activeFieldKeys.length > 0 && (
            <button
              type="button"
              onClick={() => setFilter((prev) => ({ ...prev, fieldEq: {} }))}
              className="text-[10px] ml-1 text-pc-accent hover:underline"
            >
              clear
            </button>
          )}
        </div>
      )}

      {error && (
        <div className="px-6 py-2 text-xs border-b border-status-error/20 bg-status-error/10 text-status-error">
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-4 space-y-1 min-h-0">
        {events.length === 0 && !loading ? (
          <div className="flex flex-col items-center justify-center h-full text-pc-text-muted">
            <Activity className="h-10 w-10 mb-3 text-pc-text-faint" />
            <p className="text-sm">No events match the current filters.</p>
          </div>
        ) : (
          events.map((event) => (
            <LogRow
              key={event.id}
              event={event}
              onFilterAction={setActionFilter}
              onFilterField={setFieldEq}
            />
          ))
        )}
        {!atEnd && events.length > 0 && (
          <div className="flex justify-center pt-3">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void loadOlder()}
              disabled={loadingOlder || !cursorOlder}
            >
              {loadingOlder ? 'Loading…' : 'Load older'}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

// A click-to-filter attribute value. Renders `key=value` where the value is a
// button that sets the matching filter on click. Styled to read as plain text
// until hovered/focused, so the affordance is discoverable without adding chrome
// to every row. `title` spells out what the click does for pointer + AT users.
function FilterableValue({
  attrKey,
  value,
  onClick,
}: {
  attrKey: string;
  value: string;
  onClick: () => void;
}) {
  return (
    <span>
      <span className="text-pc-text-faint">{attrKey}=</span>
      <button
        type="button"
        onClick={onClick}
        title={`Filter logs where ${attrKey} = ${value}`}
        className="rounded-[var(--radius-sm)] px-0.5 -mx-0.5 text-pc-text-muted transition-colors hover:bg-pc-accent/10 hover:text-pc-accent focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-pc-accent cursor-pointer"
      >
        {value}
      </button>
    </span>
  );
}

function LogRow({
  event,
  onFilterAction,
  onFilterField,
}: {
  event: LogEvent;
  onFilterAction: (value: string) => void;
  onFilterField: (key: string, value: string) => void;
}) {
  const level = severityClasses(event.severity_number);
  const attribution = event.zeroclaw ?? {};
  const attributionEntries = Object.entries(attribution).filter(
    ([key, value]) => key !== 'duration_ms' && value !== '' && value !== null,
  );
  const hasMessage = typeof event.message === 'string' && event.message.length > 0;
  return (
    <div className="rounded-[var(--radius-md)] px-3 py-2 border border-pc-border bg-pc-code text-xs font-mono">
      <div className="flex items-start gap-3">
        <span className="whitespace-nowrap mt-0.5 text-[10px] text-pc-text-faint">
          {formatTimestamp(event['@timestamp'])}
        </span>
        <span
          className={`inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-semibold border flex-shrink-0 ${level.chip}`}
        >
          {event.severity_text}
        </span>
        {/* category.action — the action segment is click-to-filter, populating
            the dedicated event.action filter. Category stays plain to avoid
            implying a filter that doesn't exist as a top-level control. */}
        <span className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] border border-pc-border bg-pc-base text-pc-text-muted flex-shrink-0">
          {event.event.category}.
          <button
            type="button"
            onClick={() => onFilterAction(event.event.action)}
            title={`Filter logs where event.action = ${event.event.action}`}
            className="rounded-[var(--radius-sm)] px-0.5 -mx-0.5 transition-colors hover:bg-pc-accent/10 hover:text-pc-accent focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-pc-accent cursor-pointer"
          >
            {event.event.action}
          </button>
        </span>
        <div className="flex-1 min-w-0">
          {hasMessage && (
            <p className={`text-sm break-words font-sans ${level.text}`}>
              {event.message}
            </p>
          )}
          {attributionEntries.length > 0 && (
            <div
              className={`${hasMessage ? 'mt-1' : ''} flex flex-wrap gap-x-3 gap-y-0.5 text-[10px] text-pc-text-muted`}
            >
              {attributionEntries.map(([key, value]) => (
                <FilterableValue
                  key={key}
                  attrKey={key}
                  value={String(value)}
                  onClick={() => onFilterField(key, String(value))}
                />
              ))}
              {typeof attribution.duration_ms === 'number' && (
                <span>
                  <span className="text-pc-text-faint">duration_ms=</span>
                  {attribution.duration_ms}
                </span>
              )}
            </div>
          )}
          {event.attributes && Object.keys(event.attributes).length > 0 && (
            <details className="mt-1">
              <summary className="cursor-pointer text-[10px] text-pc-text-faint">
                attributes ({Object.keys(event.attributes).length})
              </summary>
              <pre className="mt-1 p-2 rounded text-[10px] overflow-x-auto border border-pc-border bg-pc-base text-pc-text-muted">
                {JSON.stringify(event.attributes, null, 2)}
              </pre>
            </details>
          )}
        </div>
      </div>
    </div>
  );
}
