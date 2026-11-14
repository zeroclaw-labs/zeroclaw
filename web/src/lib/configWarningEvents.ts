import type { ValidationWarning } from './api';
import { getOidcSession, getToken, subscribeOidcSession } from './auth';

const CONFIG_WARNINGS_EVENT = 'zeroclaw-config-warnings';

/** A delayed write response must not reveal warnings to a changed session. */
export function captureConfigWarningAuthority(): { isCurrent: () => boolean; dispose: () => void } {
  const session = getOidcSession();
  const token = session ? null : getToken();
  let unchanged = true;
  const dispose = subscribeOidcSession(() => { unchanged = false; });
  return {
    isCurrent: () => unchanged && getOidcSession() === session && (session !== null || getToken() === token),
    dispose,
  };
}

export function subscribeConfigWarningAuthority(listener: () => void): () => void {
  const unsubscribe = subscribeOidcSession(listener);
  window.addEventListener('storage', listener);
  return () => {
    unsubscribe();
    window.removeEventListener('storage', listener);
  };
}

/** Notify the persistent dashboard surface when a caller does not render warnings locally. */
export function announceConfigWarnings(warnings?: ValidationWarning[]): void {
  if (!warnings?.length) return;
  window.dispatchEvent(new window.CustomEvent(CONFIG_WARNINGS_EVENT, { detail: warnings }));
}

export function subscribeConfigWarnings(
  listener: (warnings: ValidationWarning[]) => void,
): () => void {
  const onWarnings = (event: Event) => {
    const warnings: unknown = (event as CustomEvent<unknown>).detail;
    if (!Array.isArray(warnings)) return;
    const valid = warnings.filter((warning): warning is ValidationWarning =>
      warning !== null && typeof warning === 'object' &&
      typeof warning.code === 'string' && typeof warning.path === 'string' &&
      typeof warning.message === 'string',
    );
    if (valid.length) listener(valid);
  };
  window.addEventListener(CONFIG_WARNINGS_EVENT, onWarnings);
  return () => window.removeEventListener(CONFIG_WARNINGS_EVENT, onWarnings);
}
