// Tauri detection utilities and invoke wrappers for ZeroClaw Desktop.

import type { DesktopSettings, GatewayInfo, PermissionStatus } from '../types/api';

declare global {
  interface Window {
    __TAURI__?: unknown;
    __TAURI_INTERNALS__?: {
      invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
    };
    __ZEROCLAW_GATEWAY__?: string;
  }
}

/** Returns true when running inside a Tauri WebView. */
export const isTauri = (): boolean => '__TAURI__' in window;

/** Gateway base URL when running inside Tauri (defaults to localhost). */
export const tauriGatewayUrl = (): string =>
  window.__ZEROCLAW_GATEWAY__ ?? 'http://127.0.0.1:42617';

// ---------------------------------------------------------------------------
// Tauri invoke helpers — only call these when isTauri() is true
// ---------------------------------------------------------------------------

/** Call a Tauri command using the internal IPC bridge. */
async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // Use the Tauri internals directly to avoid needing @tauri-apps/api as a dep.
  const internals = window.__TAURI_INTERNALS__;
  if (!internals) {
    throw new Error('Tauri IPC not available');
  }
  return internals.invoke(cmd, args) as Promise<T>;
}

// ── Desktop Settings ───────────────────────────────────────────────────────

export function getDesktopSettings(): Promise<DesktopSettings> {
  return invoke<DesktopSettings>('get_desktop_settings');
}

export function setDesktopSetting(key: string, value: boolean): Promise<void> {
  return invoke<void>('set_desktop_setting', { key, value });
}

export function toggleGateway(): Promise<boolean> {
  return invoke<boolean>('toggle_gateway');
}

export function getGatewayInfo(): Promise<GatewayInfo> {
  return invoke<GatewayInfo>('get_gateway_info');
}

export function setLaunchAtLogin(enabled: boolean): Promise<void> {
  return invoke<void>('set_launch_at_login', { enabled });
}

// ── Permissions ────────────────────────────────────────────────────────────

export function getPermissionsStatus(): Promise<PermissionStatus[]> {
  return invoke<PermissionStatus[]>('get_permissions_status');
}

export function requestPermission(name: string): Promise<string> {
  return invoke<string>('request_permission', { name });
}

export function openPrivacySettings(pane: string): Promise<void> {
  return invoke<void>('open_privacy_settings', { pane });
}

// ── App Info ───────────────────────────────────────────────────────────────

export async function getAppVersion(): Promise<string> {
  try {
    return await invoke<string>('plugin:app|version');
  } catch {
    return 'unknown';
  }
}
