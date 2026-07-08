import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Activity, ChevronDown, ChevronUp, Pause, Play, Plus, RefreshCw, X } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import type { LogEvent, LogsQueryParams, LogsResponse } from '@/lib/api';
import { usePolling } from '@/hooks/usePolling';
import { Badge, Button, PageHeader } from '@/components/ui';
import { t } from '@/lib/i18n';

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
  { label: '', value: '' },
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
  options: {
    sinceTs?: string;
    untilTs?: string;
    untilId?: string;
    untilLineOffset?: number;
  } = {},
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
  if (options.untilLineOffset !== undefined) {
    params.until_line_offset = options.untilLineOffset;
  }
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
  // Prefer the byte-offset cursor returned by `next_cursor_line_offset`
  // because it is independent of id ordering and avoids the legacy
  // `(until_ts, until_id)` tie-break that can drop earlier-written
  // events when ids are written in non-lexicographic order. Fall back
  // to the legacy `[timestamp, id]` cursor when the daemon has not been
  // upgraded to expose the byte-offset field. The state is normalized
  // to `number | null` on assignment (omitted vs explicit-null both
  // deserialize to `null`) so `loadOlder`'s `!== null` check treats an
  // old-daemon omitted field the same as an explicit-null field and
  // routes through the legacy cursor branch instead of no-cursor.
  const [cursorOlderOffset, setCursorOlderOffset] = useState<number | null>(null);
  const [cursorOlderLegacy, setCursorOlderLegacy] = useState<[string, string] | null>(null);
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
  // Monotonic request-id for initialLoad. Changing the filter quickly can let
  // an older fetch resolve last; we stamp each call and only apply its results
  // if it's still the latest, so a superseded request can't clobber the list
  // (or cursor/atEnd/error) with rows that don't match the current filter.
  const loadSeqRef = useRef(0);

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
    const seq = ++loadSeqRef.current;
    setLoading(true);
    setError(null);
    try {
      const sinceTs = filterRef.current.sinceDaemonStart
        ? daemonStartedAtRef.current || undefined
        : undefined;
      const response = await fetchLogs(buildQueryParams(filterRef.current, { sinceTs }));
      // Superseded by a newer load (e.g. filter changed mid-flight): drop the
      // result entirely so we never overwrite the list with stale rows.
      if (seq !== loadSeqRef.current) return;
      setEvents(response.events);
      // Normalize omitted vs explicit-null: older daemons omit the
      // field entirely (JSON.parse → `undefined`), newer daemons emit
      // explicit `null`. `loadOlder`'s `!== null` check must treat both
      // the same way — fall back to the legacy cursor — so a missing
      // byte-offset field doesn't degrade pagination to "no cursor".
      setCursorOlderOffset(response.next_cursor_line_offset ?? null);
      setCursorOlderLegacy(response.next_cursor);
      setAtEnd(response.at_end);
      setAttributionKeys(response.attribution_keys ?? []);
      setDaemonStartedAt(response.daemon_started_at);
    } catch (err) {
      if (seq !== loadSeqRef.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      // Only the latest request owns the loading flag; a superseded request
      // clearing it would falsely signal completion while a newer one runs.
      if (seq === loadSeqRef.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void initialLoad();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // One incremental fetch — fetch newer-than-newest, append. Exposed via
  // a ref so the Pause/Resume button can fire it inline on Resume to
  // close the gap immediately instead of waiting up to POLL_INTERVAL_MS
  // for the next scheduled tick. The `isStale` guard (supplied by usePolling
  // for scheduled ticks, no-op for the manual Resume call) prevents a slow
  // in-flight request from writing after the effect was torn down/re-armed.
  const tickRef = useRef<() => Promise<void>>(async () => {});
  const tick = useCallback(
    async (isStale: () => boolean = () => false) => {
      const newest = eventsRef.current[0];
      const sinceTs = newest
        ? newest['@timestamp']
        : daemonStartedAtRef.current || undefined;
      try {
        const response = await fetchLogs(
          buildQueryParams(filterRef.current, { sinceTs }),
        );
        if (isStale()) return;
        if (response.events.length > 0) mergeNewer(response.events);
        if (response.daemon_started_at) setDaemonStartedAt(response.daemon_started_at);
        if (response.attribution_keys?.length) setAttributionKeys(response.attribution_keys);
      } catch {
        // Polling errors are silent — they'd cascade otherwise. Manual
        // Refresh surfaces errors prominently.
      }
    },
    [mergeNewer],
  );
  // Keep the manual Resume entry point pointed at the latest tick. Resume
  // clears pausedRef and calls this directly to close the pause gap inline.
  tickRef.current = () => tick();
  // usePolling owns the interval + document.hidden skip + visibilitychange
  // catch-up; we only layer on the Logs-specific "skip while paused" gate so
  // scheduled ticks don't fetch while the stream is held.
  usePolling(
    (isStale) => {
      if (pausedRef.current) return;
      return tick(isStale);
    },
    POLL_INTERVAL_MS,
    [tick],
  );

  const loadOlder = useCallback(async () => {
    // Prefer the byte-offset cursor (independent of id ordering);
    // fall back to the legacy `[timestamp, id]` pair when the daemon
    // has not been upgraded to expose `next_cursor_line_offset`.
    const hasOffsetCursor = cursorOlderOffset !== null;
    const hasLegacyCursor = cursorOlderLegacy !== null;
    if (!hasOffsetCursor && !hasLegacyCursor) return;
    if (atEnd || loadingOlder) return;
    setLoadingOlder(true);
    setError(null);
    try {
      const params = buildQueryParams(filterRef.current, {});
      if (hasOffsetCursor) {
        params.until_line_offset = cursorOlderOffset!;
      } else if (hasLegacyCursor) {
        params.until_ts = cursorOlderLegacy![0];
        params.until_id = cursorOlderLegacy![1];
      }
      const response = await fetchLogs(params);
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
      // See `initialLoad` for the omitted-vs-null normalization rationale.
      setCursorOlderOffset(response.next_cursor_line_offset ?? null);
      setCursorOlderLegacy(response.next_cursor);
      setAtEnd(response.at_end);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingOlder(false);
    }
  }, [atEnd, cursorOlderOffset, cursorOlderLegacy, loadingOlder]);

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
              {t('logs.title')}
            </span>
          }
          actions={
            <>
              <Badge tone="neutral">
                {events.length} {t('logs.events')}
                {atEnd ? ` · ${t('logs.at_end')}` : ''}
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
                    <Play className="h-3.5 w-3.5" /> {t('logs.resume')}
                  </>
                ) : (
                  <>
                    <Pause className="h-3.5 w-3.5" /> {t('logs.pause')}
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
                {t('common.refresh')}
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
          placeholder={t('logs.search_placeholder')}
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
              {option.label || t('logs.severity_any')}
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
              {option || t('logs.any_category')}
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
              {option || t('logs.any_outcome')}
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
          {t('logs.hide_internal')}
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
          {t('logs.since_daemon_start')}
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
                aria-label={`${t('logs.remove_filter_prefix')}${key}${t('logs.remove_filter_suffix')}`}
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
                {t('logs.pick_a_key')}
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
              <Plus className="h-3 w-3" /> {t('logs.add_filter')}
            </button>
          )}
          {activeFieldKeys.length > 0 && (
            <button
              type="button"
              onClick={() => setFilter((prev) => ({ ...prev, fieldEq: {} }))}
              className="text-[10px] ml-1 text-pc-accent hover:underline"
            >
              {t('logs.clear_filters')}
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
            <p className="text-sm">{t('logs.no_events')}</p>
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
              disabled={loadingOlder || (cursorOlderOffset === null && cursorOlderLegacy === null)}
            >
              {loadingOlder ? t('common.loading') : t('logs.load_older')}
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
        title={`${t('logs.filter_where_prefix')}${attrKey} = ${value}`}
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
            title={`${t('logs.filter_where_prefix')}event.action = ${event.event.action}`}
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
                {t('logs.attributes')} ({Object.keys(event.attributes).length})
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
