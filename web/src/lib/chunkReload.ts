// Each dashboard page's code loads on its first visit. A gateway that is
// stopping for a reload refuses that request (503 `gateway_restarting`), and
// a browser keeps a failed module import failed for the life of the page, so
// only a reload loads it again.

const RELOAD_KEY = 'zeroclaw_page_reload_at';
/** A second failure this soon after a reload shows the error instead. */
export const PAGE_RELOAD_GUARD_MS = 30_000;
/** The gateway's `Retry-After` for a reload. */
const RELOAD_DELAY_MS = 1_000;

export interface PageReloadDeps {
  storage: Pick<Storage, 'getItem' | 'setItem'> | null;
  reload: () => void;
  now: () => number;
  schedule: (fn: () => void, ms: number) => void;
}

function sessionStore(): PageReloadDeps['storage'] {
  try {
    return typeof sessionStorage === 'undefined' ? null : sessionStorage;
  } catch {
    return null;
  }
}

const defaultDeps = (): PageReloadDeps => ({
  storage: sessionStore(),
  reload: () => window.location.reload(),
  now: Date.now,
  schedule: (fn, ms) => {
    setTimeout(fn, ms);
  },
});

/**
 * Record a reload now, unless one happened within the guard. Without storage
 * to record it in, there is no guard, so there is no reload either.
 */
function claimReload({ storage, now }: PageReloadDeps): boolean {
  if (!storage) return false;
  try {
    const last = Number(storage.getItem(RELOAD_KEY) ?? Number.NaN);
    if (Number.isFinite(last) && now() - last < PAGE_RELOAD_GUARD_MS) return false;
    storage.setItem(RELOAD_KEY, String(now()));
    return true;
  } catch {
    return false;
  }
}

/**
 * Load a page's code. If that fails, reload the page once after a second and
 * keep the page loading until then; a second failure soon after is thrown, so
 * the page shows its error instead of reloading in a loop.
 */
export function loadPageOrReload<T>(
  load: () => Promise<T>,
  deps: PageReloadDeps = defaultDeps(),
): Promise<T> {
  return load().catch((err: unknown) => {
    if (!claimReload(deps)) throw err;
    deps.schedule(deps.reload, RELOAD_DELAY_MS);
    return new Promise<T>(() => {});
  });
}
