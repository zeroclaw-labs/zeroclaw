export const NONE_SENTINEL = '__none__';
export const APPROVAL_WILDCARD = '*';

export type AuthState = 'deny' | 'inherit' | 'allow';
export type ApprState = 'ask' | 'inherit' | 'auto';
export type CustomPermissionTarget = AuthState | ApprState;

/** Profile autonomy level. Mirrors the runtime `AutonomyLevel` enum, whose
 *  serde representation lowercases the variants (`readonly` / `supervised` /
 *  `full`). The level takes precedence over the per-tool approval lists in the
 *  runtime, so the grid must honor it to display the effective state. */
export type AutonomyLevel = 'readonly' | 'supervised' | 'full';

export interface ToolPermissionGridValue {
  allowedTools: string[];
  excludedTools: string[];
  autoApprove: string[];
  alwaysAsk: string[];
}

/** Risk-profile permission arrays contain agent-callable tool names. The
 * shared picker catalog also contains executables discovered on PATH for SOP
 * and shell-oriented pickers; those names are not tools in the runtime
 * registry and must not be offered as authorization or approval entries. */
export function filterPermissionCatalogEntries<
  T extends { group: 'agent' | 'cli' },
>(entries: readonly T[]): T[] {
  return entries.filter((entry) => entry.group === 'agent');
}

export function realAllowedTools(allowedTools: string[]): string[] {
  return allowedTools.filter((name) => name !== NONE_SENTINEL);
}

export function isMcpToolName(name: string): boolean {
  return name !== NONE_SENTINEL && name.includes('__');
}

export function isApprovalOnlyWildcard(name: string): boolean {
  return name === APPROVAL_WILDCARD;
}

export function effectiveAuthState({
  name,
  strict,
  realAllowSet,
  excludedSet,
}: {
  name: string;
  strict: boolean;
  realAllowSet: ReadonlySet<string>;
  excludedSet: ReadonlySet<string>;
}): AuthState {
  if (excludedSet.has(name)) return 'deny';
  if (strict && (realAllowSet.has(name) || isMcpToolName(name))) return 'allow';
  return 'inherit';
}

export function isMcpAutoAdmitted({
  name,
  strict,
  realAllowSet,
  excludedSet,
}: {
  name: string;
  strict: boolean;
  realAllowSet: ReadonlySet<string>;
  excludedSet: ReadonlySet<string>;
}): boolean {
  return (
    strict &&
    isMcpToolName(name) &&
    !realAllowSet.has(name) &&
    !excludedSet.has(name)
  );
}

export function effectiveApprovalState({
  name,
  autoApproveSet,
  alwaysAskSet,
}: {
  name: string;
  autoApproveSet: ReadonlySet<string>;
  alwaysAskSet: ReadonlySet<string>;
}): ApprState {
  if (alwaysAskSet.has(APPROVAL_WILDCARD) || alwaysAskSet.has(name)) return 'ask';
  if (autoApproveSet.has(APPROVAL_WILDCARD) || autoApproveSet.has(name)) return 'auto';
  return 'inherit';
}

/** Normalize a raw config `level` string into a known [`AutonomyLevel`].
 *  Unknown or empty values fall back to `supervised` - the runtime default and
 *  the only level under which the per-tool approval lists are live, so an
 *  unreadable level never silently hides a real prompt state. */
export function normalizeAutonomyLevel(raw: string | null | undefined): AutonomyLevel {
  if (raw === 'full') return 'full';
  if (raw === 'readonly') return 'readonly';
  return 'supervised';
}

/** Resolve the autonomy level for a permission group's grid from the config
 *  draft. The level lives at the group parent's `.level` sibling leaf (e.g.
 *  `risk_profiles.<alias>.level`); this is the single derivation FieldForm uses
 *  to supply the grid's `level` prop, so the draft -> level wiring is covered by
 *  testing this function rather than only the display helper. */
export function profileLevelFromDraft(
  draft: Readonly<Record<string, string>>,
  parent: string,
): AutonomyLevel {
  return normalizeAutonomyLevel(draft[`${parent}.level`]);
}

/** Which autonomy level (if any) adds a caveat to the per-tool approval lists.
 *
 *  Under `full` / `readonly` the runtime bypasses approval PROMPTS regardless of
 *  these lists (`full` auto-approves, `readonly` blocks mutating tools). But the
 *  stored entries are NOT inert: they remain valid config and still take effect
 *  on other runtime paths - for example a non-empty `always_ask` still refuses
 *  independent delegation, with no autonomy-level check. So the grid keeps the
 *  approval control LIVE (editable, clearable) and surfaces a caveat, rather than
 *  locking it to an effective value - which would both overstate the level's
 *  reach and trap an operator who needs to clear a still-load-bearing entry.
 *
 *  `supervised` -> no caveat (the lists are fully live and drive prompting). */
export type ApprovalLevelCaveat = 'full' | 'readonly' | null;

export function approvalLevelCaveat(level: AutonomyLevel): ApprovalLevelCaveat {
  return level === 'supervised' ? null : level;
}

export function isAlwaysAskWildcardLocked({
  name,
  alwaysAskSet,
}: {
  name: string;
  alwaysAskSet: ReadonlySet<string>;
}): boolean {
  return name !== APPROVAL_WILDCARD && alwaysAskSet.has(APPROVAL_WILDCARD);
}

export function applyAuthState(
  value: ToolPermissionGridValue,
  name: string,
  next: AuthState,
  strict: boolean,
): ToolPermissionGridValue {
  if (isApprovalOnlyWildcard(name)) return value;

  const nextExcluded = value.excludedTools.filter((item) => item !== name);
  const nextRealAllow = new Set(realAllowedTools(value.allowedTools));
  nextRealAllow.delete(name);

  if (next === 'deny') {
    nextExcluded.push(name);
  } else if (next === 'allow') {
    nextRealAllow.add(name);
  }

  const nextAllowedTools = strict
    ? nextRealAllow.size > 0
      ? [...nextRealAllow]
      : [NONE_SENTINEL]
    : [];

  return {
    ...value,
    excludedTools: nextExcluded,
    allowedTools: nextAllowedTools,
  };
}

export function applyApprovalState(
  value: ToolPermissionGridValue,
  name: string,
  next: ApprState,
): ToolPermissionGridValue {
  const nextAlwaysAsk = value.alwaysAsk.filter((item) => item !== name);
  const nextAutoApprove = value.autoApprove.filter((item) => item !== name);

  if (next === 'ask') nextAlwaysAsk.push(name);
  else if (next === 'auto') nextAutoApprove.push(name);

  return {
    ...value,
    alwaysAsk: nextAlwaysAsk,
    autoApprove: nextAutoApprove,
  };
}

export function applyStrictMode(
  value: ToolPermissionGridValue,
  nextStrict: boolean,
): ToolPermissionGridValue {
  const nextRealAllow = realAllowedTools(value.allowedTools);
  return {
    ...value,
    allowedTools: nextStrict
      ? nextRealAllow.length > 0
        ? nextRealAllow
        : [NONE_SENTINEL]
      : [],
  };
}

export function applyCustomPermission(
  value: ToolPermissionGridValue,
  rawName: string,
  target: CustomPermissionTarget,
): ToolPermissionGridValue | null {
  const name = rawName.trim();
  if (name.length === 0 || name === NONE_SENTINEL) return null;

  if (isApprovalOnlyWildcard(name) && (target === 'deny' || target === 'allow')) {
    return null;
  }

  if (target === 'deny') {
    return applyAuthState(value, name, 'deny', value.allowedTools.length > 0);
  }
  if (target === 'allow') {
    return {
      ...applyAuthState(value, name, 'allow', true),
      excludedTools: value.excludedTools.filter((item) => item !== name),
    };
  }
  if (target === 'ask') {
    return applyApprovalState(value, name, 'ask');
  }

  return applyApprovalState(value, name, 'auto');
}
