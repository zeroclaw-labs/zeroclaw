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
