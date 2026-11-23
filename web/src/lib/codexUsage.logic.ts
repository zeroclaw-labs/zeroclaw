// On-demand Codex subscription usage: pure state logic, no React, no network
// module imports, so it runs under plain `node --test`.
//
// This is the allowance already spent on the shared server-side Codex OAuth
// account. It is not model token or cost reporting.

export interface CodexUsageWindow {
  /** `null` means "not reported". It is never a stand-in for 0. */
  used_percent: number | null;
  /** RFC 3339 instant, or `null` when not reported. */
  resets_at: string | null;
}

export interface CodexUsageOk {
  status: 'ok';
  /** The route the server resolved and asked about (`family.alias`). */
  route: string;
  checked_at: string;
  /** `null` means the server did not report this window. */
  five_hour: CodexUsageWindow | null;
  weekly: CodexUsageWindow | null;
}

export interface CodexUsageUnavailable {
  status: 'unavailable';
  reason: string;
}

export type CodexUsageResponse = CodexUsageOk | CodexUsageUnavailable;

export type CodexUsageFetcher = () => Promise<unknown>;

export type CodexUsageProblem =
  | 'switching'
  | 'route_unavailable'
  | 'unsupported_route'
  | 'not_signed_in'
  | 'token_expired'
  | 'unavailable'
  | 'not_permitted'
  | 'request_failed';

export type CodexUsageState =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'ok'; usage: CodexUsageOk }
  | { kind: 'problem'; problem: CodexUsageProblem };

const KNOWN_REASONS: ReadonlySet<string> = new Set([
  'route_unavailable',
  'unsupported_route',
  'not_signed_in',
  'token_expired',
  'unavailable',
  'not_permitted',
]);

/** At most one decimal, trailing `.0` dropped; `0` stays `0`. */
export function formatPercent(percent: number): string {
  const rounded = percent.toFixed(1);
  return rounded.endsWith('.0') ? rounded.slice(0, -2) : rounded;
}

function parseWindow(value: unknown): CodexUsageWindow | null | undefined {
  if (value === null) return null; // valid, unreported window
  if (typeof value !== 'object') return undefined;
  const { used_percent, resets_at } = value as Record<string, unknown>;
  if (used_percent !== null &&
      (typeof used_percent !== 'number' || !Number.isFinite(used_percent) || used_percent < 0)) return undefined;
  if (resets_at !== null && typeof resets_at !== 'string') return undefined;
  return { used_percent, resets_at };
}

/** Narrow an untrusted reply; anything else is a failed request, never a guess. */
export function parseCodexUsage(value: unknown): CodexUsageResponse | null {
  if (typeof value !== 'object' || value === null) return null;
  const v = value as Record<string, unknown>;
  if (v.status === 'ok') {
    const five_hour = parseWindow(v.five_hour);
    const weekly = parseWindow(v.weekly);
    if (typeof v.route !== 'string' || !v.route || typeof v.checked_at !== 'string' ||
        five_hour === undefined || weekly === undefined || (five_hour === null && weekly === null)) return null;
    return { status: 'ok', route: v.route, checked_at: v.checked_at, five_hour, weekly };
  }
  if (v.status === 'unavailable' && typeof v.reason === 'string') {
    return {
      status: 'unavailable',
      reason: v.reason,
    };
  }
  return null;
}

/** A result is valid only for this panel, until the next check or reset. */
export class CodexUsageChecker {
  private seq = 0;
  private current: CodexUsageState = { kind: 'idle' };
  private readonly listeners = new Set<() => void>();

  private readonly fetchUsage: CodexUsageFetcher;

  constructor(fetchUsage: CodexUsageFetcher) {
    this.fetchUsage = fetchUsage;
  }

  get state(): CodexUsageState {
    return this.current;
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  private set(next: CodexUsageState): void {
    this.current = next;
    for (const listener of [...this.listeners]) listener();
  }

  reset(): void {
    this.seq += 1;
    if (this.current.kind !== 'idle') this.set({ kind: 'idle' });
  }

  async check(options: { connected: boolean; switching: boolean }): Promise<void> {
    const mine = ++this.seq;
    if (options.switching || !options.connected) {
      this.set({ kind: 'problem', problem: options.switching ? 'switching' : 'route_unavailable' });
      return;
    }
    // Drop any prior account data as soon as a refresh starts. Even an
    // unchanged route alias can name a different account after re-linking.
    this.set({ kind: 'loading' });
    let response: CodexUsageResponse | null;
    try {
      response = parseCodexUsage(await this.fetchUsage());
    } catch {
      response = null;
    }
    if (mine !== this.seq) return;
    if (response === null) {
      this.set({ kind: 'problem', problem: 'request_failed' });
    } else if (response.status === 'ok') {
      // The control frame is bound to the live chat socket, not the model
      // picker's saved default. Display the actual route alongside the data.
      this.set({ kind: 'ok', usage: response });
    } else {
      const problem = KNOWN_REASONS.has(response.reason)
        ? (response.reason as CodexUsageProblem)
        : 'unavailable';
      this.set({ kind: 'problem', problem });
    }
  }
}
