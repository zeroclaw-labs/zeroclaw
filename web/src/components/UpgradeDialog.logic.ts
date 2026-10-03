import type { StatusResponse } from '../types/api';

export type UpgradeRestartMode = NonNullable<StatusResponse['restart_mode']>;

export function canAutoRestart(
  restartMode: UpgradeRestartMode | undefined,
): boolean {
  return (
    restartMode === 'desktop_supervised' ||
    restartMode === 'supervised' ||
    restartMode === 'self_respawn'
  );
}

/**
 * The i18n key explaining why the Upgrade button is unavailable, or null when
 * an upgrade may be applied from the dashboard. A kernel installed by ZeroClaw
 * Desktop is upgraded by updating the desktop app, whatever
 * `gateway.allow_self_upgrade` says.
 */
export function upgradeBlockedMessageKey(
  allowSelfUpgrade: boolean,
  desktopBundled: boolean,
): string | null {
  if (desktopBundled) return 'upgrade.desktop_bundled';
  if (!allowSelfUpgrade) return 'upgrade.disabled';
  return null;
}
