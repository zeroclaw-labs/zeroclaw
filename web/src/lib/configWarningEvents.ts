import type { ValidationWarning } from './api';
import {
  getOidcSession, getToken, PAIRING_TOKEN_STORAGE_KEY, subscribeOidcSession, subscribePairingToken,
} from './auth';

const CONFIG_WARNINGS_EVENT = 'zeroclaw-config-warnings';

/** A delayed write response must not reveal warnings to a changed session. */
export function captureConfigWarningAuthority(): { isCurrent: () => boolean; dispose: () => void } {
  const session = getOidcSession();
  const token = session ? null : getToken();
  let unchanged = true;
  const dispose = subscribeConfigWarningAuthority(() => { unchanged = false; });
  return {
    isCurrent: () => unchanged && getOidcSession() === session && (session !== null || getToken() === token),
    dispose,
  };
}

export function subscribeConfigWarningAuthority(listener: () => void): () => void {
  const unsubscribeSession = subscribeOidcSession(listener);
  const onPairingChange = () => { if (getOidcSession() === null) listener(); };
  const unsubscribeToken = subscribePairingToken(onPairingChange);
  const unsubscribe = () => { unsubscribeSession(); unsubscribeToken(); };
  if (typeof window === 'undefined' || typeof window.addEventListener !== 'function') {
    return unsubscribe;
  }
  const onStorage = (event: StorageEvent) => {
    if (event.key === null || event.key === PAIRING_TOKEN_STORAGE_KEY) onPairingChange();
  };
  window.addEventListener('storage', onStorage);
  return () => {
    unsubscribe();
    window.removeEventListener('storage', onStorage);
  };
}

/** Reconcile the persistent surface after a successful config write. */
export function announceConfigWarnings(warnings: ValidationWarning[] | undefined, announce: boolean): void {
  if (typeof window === 'undefined' || typeof window.CustomEvent !== 'function' ||
      typeof window.dispatchEvent !== 'function') return;
  window.dispatchEvent(new window.CustomEvent(CONFIG_WARNINGS_EVENT, {
    detail: { warnings: warnings ?? [], announce },
  }));
}

export function subscribeConfigWarnings(
  listener: (warnings: ValidationWarning[], announce: boolean) => void,
): () => void {
  const onWarnings = (event: Event) => {
    const detail: unknown = (event as CustomEvent<unknown>).detail;
    if (detail === null || typeof detail !== 'object' ||
        !('warnings' in detail) || !Array.isArray(detail.warnings) ||
        !('announce' in detail) || typeof detail.announce !== 'boolean') return;
    const valid = detail.warnings.filter((warning): warning is ValidationWarning =>
      warning !== null && typeof warning === 'object' &&
      typeof warning.code === 'string' && typeof warning.path === 'string' &&
      typeof warning.message === 'string',
    );
    listener(valid, detail.announce);
  };
  window.addEventListener(CONFIG_WARNINGS_EVENT, onWarnings);
  return () => window.removeEventListener(CONFIG_WARNINGS_EVENT, onWarnings);
}
