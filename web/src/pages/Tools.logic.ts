import type { PatchOp } from '../lib/api';
import {
  applyAuthState,
  isStrictAllowlist,
  type ToolPermissionGridValue,
} from '../components/ToolPermissionGrid.logic.ts';

export interface ToolAccess {
  allowed: string[] | null;
  denyAll: boolean;
  excluded: string[];
}

export interface ToolAccessPatch {
  previous: ToolAccess;
  next: ToolAccess;
  ops: PatchOp[];
}

export function buildToolAccessPatch(
  profile: string,
  tool: string,
  current: ToolAccess,
  makeAllowed: boolean,
): ToolAccessPatch | null {
  const currentValue: ToolPermissionGridValue = {
    allowedTools: current.allowed,
    denyAllTools: current.denyAll,
    excludedTools: current.excluded,
    autoApprove: [],
    alwaysAsk: [],
  };
  const nextValue = applyAuthState(
    currentValue,
    tool,
    makeAllowed ? 'allow' : 'deny',
    isStrictAllowlist(currentValue),
  );
  const next: ToolAccess = {
    allowed: nextValue.allowedTools,
    denyAll: nextValue.denyAllTools,
    excluded: nextValue.excludedTools,
  };
  const ops: PatchOp[] = [];
  if (JSON.stringify(next.allowed) !== JSON.stringify(current.allowed)) {
    ops.push({
      op: 'replace',
      path: `risk_profiles.${profile}.allowed_tools`,
      value: next.allowed,
    });
  }
  if (next.denyAll !== current.denyAll) {
    ops.push({
      op: 'replace',
      path: `risk_profiles.${profile}.deny_all_tools`,
      value: next.denyAll,
    });
  }
  if (JSON.stringify(next.excluded) !== JSON.stringify(current.excluded)) {
    ops.push({
      op: 'replace',
      path: `risk_profiles.${profile}.excluded_tools`,
      value: next.excluded.length > 0 ? next.excluded : null,
    });
  }
  return ops.length > 0 ? { previous: current, next, ops } : null;
}

export async function applyToolAccessPatch(
  change: ToolAccessPatch,
  patch: (ops: PatchOp[]) => Promise<unknown>,
  setState: (state: ToolAccess) => void,
): Promise<void> {
  setState(change.next);
  try {
    await patch(change.ops);
  } catch (error) {
    setState(change.previous);
    throw error;
  }
}
