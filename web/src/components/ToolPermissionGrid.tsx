// Reusable two-axis tool-permission grid. Each row is one tool; the operator
// sets two independent states per tool instead of picking through four
// separate lists:
//  - Authorization (deny / inherit / allow) — can the agent call this tool
//    at all. Bridges `allowed_tools` + `excluded_tools`.
//  - Approval gating (ask every time / inherit / auto-approve) — does a
//    human confirm before it runs. Bridges `auto_approve` + `always_ask`.
//
// The two axes aren't symmetric. An empty `allowed_tools` means
// *unrestricted*, not deny-all, so "Strict allowlist" is a real mode toggle
// here, not just a third row state on the Authorization axis — flipping it
// on with nothing marked Allow writes a sentinel tool name (`__none__`, the
// same convention operators were already hand-writing into `allowed_tools`
// to fake a zero-tools profile) so the config still round-trips through
// today's backend with no schema change. See `RiskProfileConfig` in
// crates/zeroclaw-config/src/schema.rs.
//
// Deny always wins its axis; Ask-every-time always wins over Auto-approve
// when a tool is in both — see crates/zeroclaw-runtime/src/approval/mod.rs.
//
// The component is controlled — it owns no permission state, just reflects
// `value` and fires `onChange(next)` with the full updated bundle.
//
// i18n: user-facing copy is routed through t() under the
// `tool_permission_grid.` namespace, reusing `tool_picker.` keys where the
// copy is identical (search placeholder, group labels, loading/error text).

import { useEffect, useMemo, useState } from 'react';
import { Search, X, Check, Minus, AlertCircle, Zap } from 'lucide-react';
import {
  loadToolCatalogResult,
  peekToolCatalog,
  type CatalogEntry,
  type CatalogLoadWarning,
} from '@/lib/toolCatalog';
import { t } from '@/lib/i18n';
import { ToolCatalogWarningPanel } from './ToolCatalogWarningPanel';
import {
  APPROVAL_WILDCARD,
  applyApprovalState,
  applyAuthState,
  applyCustomPermission,
  applyStrictMode,
  approvalLevelCaveat,
  effectiveApprovalState,
  effectiveAuthState,
  filterPermissionCatalogEntries,
  isApprovalOnlyWildcard,
  isAlwaysAskWildcardLocked,
  isMcpAutoAdmitted,
  realAllowedTools,
  type ApprState,
  type AuthState,
  type AutonomyLevel,
  type CustomPermissionTarget,
  type ToolPermissionGridValue,
} from './ToolPermissionGrid.logic';

export type { ToolPermissionGridValue } from './ToolPermissionGrid.logic';

export interface ToolPermissionGridProps {
  value: ToolPermissionGridValue;
  /** Fired with the full updated bundle — the component always resolves a
   *  single row edit into every affected list at once, so callers never
   *  have to reconcile four independent partial updates. */
  onChange: (next: ToolPermissionGridValue) => void;
  /** When true, every control is inert. */
  disabled?: boolean;
  /** DOM id base for the search input and the strict-allowlist switch. */
  id?: string;
  /** Scope the tool catalog to this agent, same as ToolPicker. */
  agent?: string;
  /** The profile's autonomy level. Under `full` / `readonly` the runtime
   *  decides approval before consulting the per-tool lists, so the approval
   *  column is shown locked with an explanatory tooltip instead of a state the
   *  runtime would not honor. Defaults to `supervised` (lists are live). */
  level?: AutonomyLevel;
}

interface Row {
  name: string;
  description: string;
  group: 'agent' | 'cli' | 'unknown';
}

export default function ToolPermissionGrid({
  value,
  onChange,
  disabled = false,
  id,
  agent,
  level = 'supervised',
}: ToolPermissionGridProps) {
  const cacheKey = agent ?? '';
  const [catalog, setCatalog] = useState<CatalogEntry[] | null>(() => peekToolCatalog(cacheKey));
  const [loading, setLoading] = useState(() => peekToolCatalog(cacheKey) === null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<CatalogLoadWarning[]>([]);
  const [reloadSeq, setReloadSeq] = useState(0);
  const [search, setSearch] = useState('');
  const [customName, setCustomName] = useState('');
  const [customError, setCustomError] = useState<string | null>(null);

  useEffect(() => {
    const cached = reloadSeq === 0 ? peekToolCatalog(cacheKey) : null;
    if (cached) {
      setCatalog(cached);
      setLoading(false);
      setError(null);
      setWarnings([]);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    setWarnings([]);
    setCatalog(null);
    loadToolCatalogResult(agent)
      .then((result) => {
        if (!cancelled) {
          setCatalog(result.entries);
          setWarnings(result.warnings);
          setLoading(false);
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : t('tool_picker.load_failed'));
          setWarnings([]);
          setCatalog([]);
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [agent, cacheKey, reloadSeq]);

  const strict = value.allowedTools.length > 0;
  const realAllowSet = useMemo(
    () => new Set(realAllowedTools(value.allowedTools)),
    [value.allowedTools],
  );
  const excludedSet = useMemo(() => new Set(value.excludedTools), [value.excludedTools]);
  const autoApproveSet = useMemo(() => new Set(value.autoApprove), [value.autoApprove]);
  const alwaysAskSet = useMemo(() => new Set(value.alwaysAsk), [value.alwaysAsk]);

  // The shared catalog includes executables discovered on PATH for callers
  // such as SOP editors. Risk-profile permission arrays are evaluated against
  // agent tool names, so keep those CLI-only entries out of this grid.
  const permissionCatalog = useMemo(
    () => filterPermissionCatalogEntries(catalog ?? []),
    [catalog],
  );
  const permissionWarnings = useMemo(
    () => warnings.filter((warning) => warning.source === 'agent'),
    [warnings],
  );

  const byName = useMemo(() => {
    const map = new Map<string, CatalogEntry>();
    for (const e of permissionCatalog) map.set(e.name, e);
    return map;
  }, [permissionCatalog]);

  // Any name referenced by one of the four lists but missing from the
  // catalog (renamed/removed tool, or an agent-scoped picker viewing a
  // profile that references a tool outside that agent's catalog) still
  // gets a row — its state shouldn't silently vanish from view.
  const unknownRows = useMemo(() => {
    const names = new Set<string>();
    for (const n of realAllowSet) if (!byName.has(n)) names.add(n);
    for (const n of value.excludedTools) if (!byName.has(n)) names.add(n);
    for (const n of value.autoApprove) if (!byName.has(n)) names.add(n);
    for (const n of value.alwaysAsk) if (!byName.has(n)) names.add(n);
    return [...names].map((name): Row => ({
      name,
      description:
        name === APPROVAL_WILDCARD
          ? t('tool_permission_grid.wildcard_desc')
          : t('tool_picker.unknown_tool_desc'),
      group: 'unknown',
    }));
  }, [byName, realAllowSet, value.excludedTools, value.autoApprove, value.alwaysAsk]);

  const catalogRows = useMemo(
    (): Row[] => permissionCatalog.map((e) => ({
      name: e.name,
      description: e.description,
      group: e.group,
    })),
    [permissionCatalog],
  );

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    const all = [...unknownRows, ...catalogRows];
    if (!q) return all;
    return all.filter(
      (r) => r.name.toLowerCase().includes(q) || r.description.toLowerCase().includes(q),
    );
  }, [unknownRows, catalogRows, search]);

  const unknownFiltered = filtered.filter((r) => r.group === 'unknown');
  const agentFiltered = filtered.filter((r) => r.group === 'agent');
  const cliFiltered = filtered.filter((r) => r.group === 'cli');

  function authState(name: string): AuthState {
    return effectiveAuthState({ name, strict, realAllowSet, excludedSet });
  }
  function apprState(name: string): ApprState {
    return effectiveApprovalState({ name, autoApproveSet, alwaysAskSet });
  }

  function setAuth(name: string, next: AuthState) {
    if (disabled || isApprovalOnlyWildcard(name)) return;
    if (next === 'allow' && !strict) return; // no-op: explicit Allow only means something in strict mode
    onChange(applyAuthState(value, name, next, strict));
  }

  function setApproval(name: string, next: ApprState) {
    if (disabled) return;
    // The control stays live under full/readonly: those levels bypass approval
    // PROMPTS, but the stored always_ask/auto_approve entries remain editable and
    // still matter on other runtime paths (e.g. always_ask blocks independent
    // delegation), so an operator must be able to clear them. The level effect is
    // surfaced by the caveat banner, not by locking the row.
    if (isAlwaysAskWildcardLocked({ name, alwaysAskSet })) return;
    onChange(applyApprovalState(value, name, next));
  }

  function setStrict(nextStrict: boolean) {
    if (disabled || nextStrict === strict) return;
    onChange(applyStrictMode(value, nextStrict));
  }

  function addCustom(target: CustomPermissionTarget) {
    if (disabled) return;
    const next = applyCustomPermission(value, customName, target);
    if (!next) {
      setCustomError(t('tool_permission_grid.add_invalid'));
      return;
    }
    setCustomError(null);
    setCustomName('');
    onChange(next);
  }

  function retryCatalogLoad() {
    setReloadSeq((seq) => seq + 1);
  }

  function mcpAutoAdmitted(name: string): boolean {
    return isMcpAutoAdmitted({ name, strict, realAllowSet, excludedSet });
  }

  function alwaysAskWildcardLocked(name: string): boolean {
    return isAlwaysAskWildcardLocked({ name, alwaysAskSet });
  }

  function autoApproveWildcardApplies(name: string): boolean {
    return (
      name !== APPROVAL_WILDCARD &&
      autoApproveSet.has(APPROVAL_WILDCARD) &&
      !alwaysAskSet.has(APPROVAL_WILDCARD) &&
      !alwaysAskSet.has(name)
    );
  }

  const authorizationRows = filtered.filter((r) => !isApprovalOnlyWildcard(r.name));
  const allowedCount = authorizationRows.filter((r) => {
    const s = authState(r.name);
    return strict ? s === 'allow' : s !== 'deny';
  }).length;

  const strictId = id ? `${id}-strict` : undefined;
  const searchId = id ? `${id}-search` : undefined;
  const listId = id ? `${id}-rows` : undefined;
  const customInputId = id ? `${id}-custom-tool` : undefined;
  const canAddCustom = !disabled && customName.trim().length > 0;
  const customNameIsApprovalOnlyWildcard = isApprovalOnlyWildcard(customName.trim());
  // full/readonly bypass approval PROMPTS, but the approval lists stay live
  // (editable) because they still take effect on other runtime paths. A visible
  // banner - not a row lock - identifies them as stored overrides.
  const levelCaveat = approvalLevelCaveat(level);

  return (
    <div className="space-y-3">
      {levelCaveat && (
        <div
          role="note"
          className="flex items-start gap-2 rounded-md border border-status-warning/40 bg-status-warning/10 px-3 py-2 text-xs text-pc-text-secondary"
        >
          <AlertCircle
            className="h-4 w-4 flex-shrink-0 text-status-warning mt-0.5"
            aria-hidden="true"
          />
          <span>
            {levelCaveat === 'full'
              ? t('tool_permission_grid.appr_level_banner_full')
              : t('tool_permission_grid.appr_level_banner_readonly')}
          </span>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 justify-between">
        <div className="flex items-center gap-2 text-sm">
          <button
            type="button"
            id={strictId}
            role="switch"
            aria-checked={strict}
            disabled={disabled}
            onClick={() => setStrict(!strict)}
            className={[
              'relative inline-flex h-[22px] w-[38px] flex-shrink-0 items-center rounded-full border transition-colors',
              strict ? 'bg-pc-accent border-pc-accent' : 'bg-pc-input border-pc-border-strong',
              disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer',
              'focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]/40',
            ].join(' ')}
          >
            <span
              className={[
                'inline-block h-4 w-4 transform rounded-full bg-pc-text transition-transform',
                strict ? 'translate-x-[18px]' : 'translate-x-0.5',
              ].join(' ')}
            />
          </button>
          <label htmlFor={strictId} className="font-medium text-pc-text-secondary cursor-pointer">
            {t('tool_permission_grid.strict_allowlist')}
          </label>
          <span className="text-xs text-pc-text-faint">
            {strict
              ? t('tool_permission_grid.strict_on_hint')
              : t('tool_permission_grid.strict_off_hint')}
          </span>
        </div>
        <span className="font-mono text-xs text-pc-text-secondary [font-variant-numeric:tabular-nums]">
          {allowedCount}/{authorizationRows.length}
          {t('tool_permission_grid.summary_suffix')}
        </span>
      </div>

      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-pc-text-faint pointer-events-none" />
        <input
          id={searchId}
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          disabled={disabled || loading}
          placeholder={t('tool_picker.search_placeholder')}
          aria-label={t('tool_picker.search_placeholder')}
          className="w-full h-9 pl-9 pr-3 text-sm rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 disabled:opacity-50 disabled:cursor-not-allowed"
        />
      </div>

      <div className="rounded-[var(--radius-md)] border border-pc-border bg-pc-input/60 p-2">
        <label htmlFor={customInputId} className="sr-only">
          {t('tool_permission_grid.add_label')}
        </label>
        <div className="flex flex-col sm:flex-row gap-2">
          <input
            id={customInputId}
            type="text"
            value={customName}
            onChange={(event) => {
              setCustomName(event.target.value);
              if (customError) setCustomError(null);
            }}
            disabled={disabled}
            placeholder={t('tool_permission_grid.add_placeholder')}
            className="min-w-0 flex-1 h-9 px-3 text-sm rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 disabled:opacity-50 disabled:cursor-not-allowed"
          />
          <div className="flex flex-wrap gap-1.5">
            <button
              type="button"
              onClick={() => addCustom('deny')}
              disabled={!canAddCustom || customNameIsApprovalOnlyWildcard}
              title={t('tool_permission_grid.add_deny_title')}
              aria-label={t('tool_permission_grid.add_deny_title')}
              className="flex h-9 w-9 items-center justify-center rounded-[var(--radius-md)] border border-status-error/30 text-status-error transition-colors hover:bg-status-error/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]/40 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <X className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              onClick={() => addCustom('allow')}
              disabled={!canAddCustom || customNameIsApprovalOnlyWildcard}
              title={t('tool_permission_grid.add_allow_title')}
              aria-label={t('tool_permission_grid.add_allow_title')}
              className="flex h-9 w-9 items-center justify-center rounded-[var(--radius-md)] border border-status-success/30 text-status-success transition-colors hover:bg-status-success/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]/40 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <Check className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              onClick={() => addCustom('ask')}
              disabled={!canAddCustom}
              title={t('tool_permission_grid.add_ask_title')}
              aria-label={t('tool_permission_grid.add_ask_title')}
              className="flex h-9 w-9 items-center justify-center rounded-[var(--radius-md)] border border-status-warning/30 text-status-warning transition-colors hover:bg-status-warning/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]/40 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <AlertCircle className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              onClick={() => addCustom('auto')}
              disabled={!canAddCustom}
              title={t('tool_permission_grid.add_auto_title')}
              aria-label={t('tool_permission_grid.add_auto_title')}
              className="flex h-9 w-9 items-center justify-center rounded-[var(--radius-md)] border border-pc-accent/30 text-pc-accent transition-colors hover:bg-pc-accent/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]/40 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <Zap className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
        {customError && (
          <p className="mt-1.5 text-xs text-status-error">{customError}</p>
        )}
      </div>

      {!loading && filtered.length > 0 && (
        <div className="hidden sm:grid grid-cols-[1fr_120px_120px] gap-3 px-3">
          <span className="text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
            {t('tool_permission_grid.col_tool')}
          </span>
          <span className="text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
            {t('tool_permission_grid.col_authorization')}
            <small className="block font-normal normal-case tracking-normal text-pc-text-faint">
              {strict
                ? t('tool_permission_grid.col_authorization_hint_blocked')
                : t('tool_permission_grid.col_authorization_hint_open')}
            </small>
          </span>
          <span className="text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
            {t('tool_permission_grid.col_approval')}
            <small className="block font-normal normal-case tracking-normal text-pc-text-faint">
              {t('tool_permission_grid.col_approval_hint')}
            </small>
          </span>
        </div>
      )}

      {error && (
        <div className="rounded-[var(--radius-md)] border border-status-warning/25 bg-status-warning/10 px-3 py-2 text-xs text-status-warning">
          {t('tool_picker.load_failed_prefix')}
          {error}
        </div>
      )}

      {permissionWarnings.length > 0 && (
        <ToolCatalogWarningPanel
          warnings={permissionWarnings}
          onRetry={retryCatalogLoad}
          retryDisabled={loading}
        />
      )}

      {loading ? (
        <div className="flex items-center gap-2 px-3 py-4 text-xs text-pc-text-muted">
          <div
            className="h-4 w-4 border-2 rounded-full animate-spin border-pc-border"
            style={{ borderTopColor: 'var(--pc-accent)' }}
          />
          {t('tool_picker.loading')}
        </div>
      ) : (
        <div
          id={listId}
          className="max-h-96 overflow-y-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-surface divide-y divide-pc-border/60"
        >
          {unknownFiltered.length > 0 && (
            <RowGroup label={t('tool_picker.group_unknown')} count={unknownFiltered.length}>
              {unknownFiltered.map((r) => (
                <PermissionRow
                  key={r.name}
                  row={r}
                  strict={strict}
                  disabled={disabled}
                  authState={authState(r.name)}
                  apprState={apprState(r.name)}
                  mcpAutoAdmitted={mcpAutoAdmitted(r.name)}
                  alwaysAskWildcardLocked={alwaysAskWildcardLocked(r.name)}
                  autoApproveWildcardApplies={autoApproveWildcardApplies(r.name)}
                  onAuth={(s) => setAuth(r.name, s)}
                  onAppr={(s) => setApproval(r.name, s)}
                />
              ))}
            </RowGroup>
          )}
          {agentFiltered.length > 0 && (
            <RowGroup label={t('tool_picker.group_agent')} count={agentFiltered.length}>
              {agentFiltered.map((r) => (
                <PermissionRow
                  key={r.name}
                  row={r}
                  strict={strict}
                  disabled={disabled}
                  authState={authState(r.name)}
                  apprState={apprState(r.name)}
                  mcpAutoAdmitted={mcpAutoAdmitted(r.name)}
                  alwaysAskWildcardLocked={alwaysAskWildcardLocked(r.name)}
                  autoApproveWildcardApplies={autoApproveWildcardApplies(r.name)}
                  onAuth={(s) => setAuth(r.name, s)}
                  onAppr={(s) => setApproval(r.name, s)}
                />
              ))}
            </RowGroup>
          )}
          {cliFiltered.length > 0 && (
            <RowGroup label={t('tool_picker.group_cli')} count={cliFiltered.length}>
              {cliFiltered.map((r) => (
                <PermissionRow
                  key={r.name}
                  row={r}
                  strict={strict}
                  disabled={disabled}
                  authState={authState(r.name)}
                  apprState={apprState(r.name)}
                  mcpAutoAdmitted={mcpAutoAdmitted(r.name)}
                  alwaysAskWildcardLocked={alwaysAskWildcardLocked(r.name)}
                  autoApproveWildcardApplies={autoApproveWildcardApplies(r.name)}
                  onAuth={(s) => setAuth(r.name, s)}
                  onAppr={(s) => setApproval(r.name, s)}
                />
              ))}
            </RowGroup>
          )}
          {filtered.length === 0 && (
            <p className="px-3 py-4 text-xs text-center text-pc-text-muted">
              {search.trim()
                ? `${t('tool_picker.no_match_prefix')}"${search.trim()}"${t('tool_picker.no_match_suffix')}`
                : t('tool_picker.no_tools_available')}
            </p>
          )}
        </div>
      )}

      <p className="text-[11px] leading-relaxed text-pc-text-faint">
        {t('tool_permission_grid.legend')}
      </p>
    </div>
  );
}

function RowGroup({
  label,
  count,
  children,
}: {
  label: string;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="sticky top-0 z-10 px-3 py-1.5 bg-pc-elevated border-b border-pc-border/60">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
          {label}
        </span>
        <span className="text-[10px] text-pc-text-faint ml-1">({count})</span>
      </div>
      <div className="divide-y divide-pc-border/40">{children}</div>
    </div>
  );
}

interface SegmentedOption<T extends string> {
  value: T;
  icon: React.ComponentType<{ className?: string }>;
  tone: 'error' | 'success' | 'warning' | 'accent' | 'neutral';
  title: string;
  optionDisabled?: boolean;
}

const TONE_CLASSES: Record<SegmentedOption<string>['tone'], string> = {
  error: 'bg-status-error/15 text-status-error',
  success: 'bg-status-success/15 text-status-success',
  warning: 'bg-status-warning/15 text-status-warning',
  accent: 'bg-pc-accent/15 text-pc-accent',
  neutral: 'bg-pc-elevated text-pc-text-secondary',
};

function Segmented<T extends string>({
  value,
  options,
  disabled,
  ariaLabel,
  onChange,
}: {
  value: T;
  options: SegmentedOption<T>[];
  disabled?: boolean;
  ariaLabel: string;
  onChange: (next: T) => void;
}) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className={[
        'inline-flex rounded-full border border-pc-border-strong bg-pc-input overflow-hidden w-fit',
        disabled ? 'opacity-50' : '',
      ].join(' ')}
    >
      {options.map((opt, i) => {
        const active = opt.value === value;
        const isDisabled = disabled || opt.optionDisabled;
        const Icon = opt.icon;
        return (
          <button
            key={opt.value}
            type="button"
            aria-pressed={active}
            aria-label={opt.title}
            disabled={isDisabled}
            title={opt.title}
            onClick={() => onChange(opt.value)}
            className={[
              'flex items-center justify-center w-[34px] h-7 transition-colors',
              i > 0 ? 'border-l border-pc-border' : '',
              isDisabled
                ? 'cursor-not-allowed opacity-40'
                : 'cursor-pointer hover:bg-pc-elevated',
              active ? TONE_CLASSES[opt.tone] : 'text-pc-text-faint',
              'focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--pc-focus)]/40',
            ].join(' ')}
          >
            <Icon className="h-3.5 w-3.5" />
          </button>
        );
      })}
    </div>
  );
}

function PermissionRow({
  row,
  authState,
  apprState,
  strict,
  disabled,
  mcpAutoAdmitted,
  alwaysAskWildcardLocked,
  autoApproveWildcardApplies,
  onAuth,
  onAppr,
}: {
  row: Row;
  authState: AuthState;
  apprState: ApprState;
  strict: boolean;
  disabled: boolean;
  mcpAutoAdmitted: boolean;
  alwaysAskWildcardLocked: boolean;
  autoApproveWildcardApplies: boolean;
  onAuth: (next: AuthState) => void;
  onAppr: (next: ApprState) => void;
}) {
  const approvalOnlyWildcard = isApprovalOnlyWildcard(row.name);
  const denied = !approvalOnlyWildcard && authState === 'deny';
  const allowDisabled = disabled || !strict;
  const approvalDisabled = disabled || denied || alwaysAskWildcardLocked;
  const authAxisLabel = `${row.name} ${t('tool_permission_grid.col_authorization')}`;
  const approvalAxisLabel = `${row.name} ${t('tool_permission_grid.col_approval')}`;

  return (
    <div className="grid grid-cols-1 sm:grid-cols-[1fr_120px_120px] items-center gap-2 sm:gap-3 px-3 py-2.5">
      <div className={denied ? 'min-w-0 opacity-55' : 'min-w-0'}>
        <div className="flex items-center gap-2">
          <span className="font-mono text-[13px] text-pc-text truncate">{row.name}</span>
          {mcpAutoAdmitted && (
            <span
              className="text-[10px] uppercase tracking-wide text-pc-accent flex-shrink-0"
              title={t('tool_permission_grid.mcp_auto_badge_title')}
            >
              {t('tool_permission_grid.mcp_auto_badge')}
            </span>
          )}
          {row.group === 'unknown' && (
            <span className="text-[10px] uppercase tracking-wide text-status-warning flex-shrink-0">
              {t('tool_picker.unknown_badge')}
            </span>
          )}
        </div>
        <p className="text-xs text-pc-text-muted mt-0.5 truncate">{row.description}</p>
      </div>

      <div className="flex items-center justify-between gap-2 sm:block">
        <span className="sm:hidden text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
          {t('tool_permission_grid.col_authorization')}
        </span>
        {approvalOnlyWildcard ? (
          <span
            className="inline-flex h-7 items-center text-xs text-pc-text-faint"
            aria-label={t('tool_permission_grid.auth_approval_only')}
          >
            {t('tool_permission_grid.auth_approval_only')}
          </span>
        ) : (
          <Segmented<AuthState>
            value={authState}
            disabled={disabled}
            ariaLabel={authAxisLabel}
            onChange={onAuth}
            options={[
              { value: 'deny', icon: X, tone: 'error', title: t('tool_permission_grid.auth_deny_title') },
              {
                value: 'inherit',
                icon: Minus,
                tone: 'neutral',
                optionDisabled: mcpAutoAdmitted,
                title: mcpAutoAdmitted
                  ? t('tool_permission_grid.auth_inherit_mcp_title')
                  : strict
                    ? t('tool_permission_grid.auth_inherit_blocked_title')
                    : t('tool_permission_grid.auth_inherit_open_title'),
              },
              {
                value: 'allow',
                icon: Check,
                tone: 'success',
                optionDisabled: allowDisabled,
                title: mcpAutoAdmitted
                  ? t('tool_permission_grid.auth_allow_mcp_title')
                  : allowDisabled
                    ? t('tool_permission_grid.auth_allow_disabled_title')
                    : t('tool_permission_grid.auth_allow_title'),
              },
            ]}
          />
        )}
      </div>

      <div className="flex items-center justify-between gap-2 sm:block">
        <span className="sm:hidden text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
          {t('tool_permission_grid.col_approval')}
        </span>
        <Segmented<ApprState>
          value={apprState}
          disabled={approvalDisabled}
          ariaLabel={approvalAxisLabel}
          onChange={onAppr}
          options={[
            {
              value: 'ask',
              icon: AlertCircle,
              tone: 'warning',
              title: denied
                ? t('tool_permission_grid.appr_moot_title')
                : alwaysAskWildcardLocked
                  ? t('tool_permission_grid.appr_wildcard_always_ask_title')
                  : t('tool_permission_grid.appr_ask_title'),
            },
            {
              value: 'inherit',
              icon: Minus,
              tone: 'neutral',
              optionDisabled: autoApproveWildcardApplies,
              title: denied
                ? t('tool_permission_grid.appr_moot_title')
                : alwaysAskWildcardLocked
                  ? t('tool_permission_grid.appr_wildcard_always_ask_title')
                  : autoApproveWildcardApplies
                    ? t('tool_permission_grid.appr_wildcard_auto_title')
                    : t('tool_permission_grid.appr_inherit_title'),
            },
            {
              value: 'auto',
              icon: Zap,
              tone: 'accent',
              title: denied
                ? t('tool_permission_grid.appr_moot_title')
                : alwaysAskWildcardLocked
                  ? t('tool_permission_grid.appr_wildcard_always_ask_title')
                  : autoApproveWildcardApplies
                    ? t('tool_permission_grid.appr_wildcard_auto_title')
                    : t('tool_permission_grid.appr_auto_title'),
            },
          ]}
        />
      </div>
    </div>
  );
}
