import { useState, useEffect, useCallback } from 'react';
import { SlidersHorizontal, RefreshCw } from 'lucide-react';
import {
  isTauri,
  getDesktopSettings,
  setDesktopSetting,
  toggleGateway,
  getGatewayInfo,
} from '../lib/tauri';
import type { DesktopSettings, GatewayInfo } from '../types/api';
import { t } from '@/lib/i18n';

interface ToggleRowProps {
  label: string;
  description: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}

function ToggleRow({ label, description, checked, onChange }: ToggleRowProps) {
  return (
    <div className="flex items-center justify-between py-3">
      <div>
        <div className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
          {label}
        </div>
        <div className="text-xs mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>
          {description}
        </div>
      </div>
      <button
        onClick={() => onChange(!checked)}
        className="relative inline-flex h-6 w-11 items-center rounded-full transition-colors"
        style={{ background: checked ? 'var(--pc-accent)' : 'var(--pc-border)' }}
        role="switch"
        aria-checked={checked}
      >
        <span
          className="inline-block h-4 w-4 rounded-full transition-transform bg-white"
          style={{ transform: checked ? 'translateX(22px)' : 'translateX(4px)' }}
        />
      </button>
    </div>
  );
}

export default function General() {
  const [settings, setSettings] = useState<DesktopSettings | null>(null);
  const [gateway, setGateway] = useState<GatewayInfo | null>(null);
  const [toggling, setToggling] = useState(false);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    try {
      const [s, g] = await Promise.all([getDesktopSettings(), getGatewayInfo()]);
      setSettings(s);
      setGateway(g);
    } catch {
      // Gateway may not be running yet.
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const handleToggleSetting = async (key: keyof DesktopSettings) => {
    if (!settings) return;
    const newVal = !settings[key];
    await setDesktopSetting(key, newVal);
    setSettings({ ...settings, [key]: newVal });
  };

  const handleToggleGateway = async () => {
    setToggling(true);
    try {
      const active = await toggleGateway();
      setGateway((g) => g ? { ...g, active, health_status: active ? 'healthy' : 'unreachable' } : g);
    } finally {
      setToggling(false);
    }
  };

  if (!isTauri()) {
    return (
      <div className="p-6">
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          General settings are only available in the desktop app.
        </p>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center gap-3">
        <SlidersHorizontal className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
        <h1 className="text-xl font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
          {t('general.title') || 'General'}
        </h1>
      </div>

      {/* Active toggle */}
      <div className="card p-5">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div
              className="h-3 w-3 rounded-full"
              style={{ background: gateway?.active ? 'var(--color-status-ok)' : 'var(--color-status-error)' }}
            />
            <div>
              <div className="text-base font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
                ZeroClaw {gateway?.active ? 'Active' : 'Inactive'}
              </div>
              <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                {gateway?.active
                  ? 'Gateway is running and processing messages.'
                  : 'Pause to stop the ZeroClaw gateway; no messages will be processed.'}
              </div>
            </div>
          </div>
          <button
            onClick={handleToggleGateway}
            disabled={toggling}
            className="relative inline-flex h-6 w-11 items-center rounded-full transition-colors"
            style={{ background: gateway?.active ? 'var(--pc-accent)' : 'var(--pc-border)' }}
            role="switch"
            aria-checked={gateway?.active ?? false}
          >
            <span
              className="inline-block h-4 w-4 rounded-full transition-transform bg-white"
              style={{ transform: gateway?.active ? 'translateX(22px)' : 'translateX(4px)' }}
            />
          </button>
        </div>
      </div>

      {/* Gateway info */}
      {gateway && (
        <div className="card p-5 space-y-2">
          <div className="text-sm font-medium mb-3" style={{ color: 'var(--pc-text-primary)' }}>
            Gateway Status
          </div>
          <div className="grid grid-cols-2 gap-3 text-xs">
            <div>
              <span style={{ color: 'var(--pc-text-muted)' }}>Port:</span>{' '}
              <span style={{ color: 'var(--pc-text-primary)' }}>{gateway.port}</span>
            </div>
            <div>
              <span style={{ color: 'var(--pc-text-muted)' }}>Health:</span>{' '}
              <span style={{ color: gateway.active ? 'var(--color-status-ok)' : 'var(--color-status-error)' }}>
                {gateway.health_status}
              </span>
            </div>
            {gateway.version && (
              <div>
                <span style={{ color: 'var(--pc-text-muted)' }}>Version:</span>{' '}
                <span style={{ color: 'var(--pc-text-primary)' }}>{gateway.version}</span>
              </div>
            )}
          </div>
          <button onClick={load} className="mt-3 text-xs flex items-center gap-1" style={{ color: 'var(--pc-accent)' }}>
            <RefreshCw className="h-3 w-3" /> Recheck
          </button>
        </div>
      )}

      {/* Settings toggles */}
      {settings && (
        <div className="card p-5">
          <div className="divide-y" style={{ borderColor: 'var(--pc-border)' }}>
            <ToggleRow
              label="Launch at Login"
              description="Automatically start ZeroClaw after you sign in."
              checked={settings.launch_at_login}
              onChange={() => handleToggleSetting('launch_at_login')}
            />
            <ToggleRow
              label="Show Dock Icon"
              description="Keep ZeroClaw visible in the Dock instead of menu-bar-only mode."
              checked={settings.show_dock_icon}
              onChange={() => handleToggleSetting('show_dock_icon')}
            />
            <ToggleRow
              label="Play Menu Bar Icon Animations"
              description="Enable idle blinks and wiggles on the status icon."
              checked={settings.play_tray_animations}
              onChange={() => handleToggleSetting('play_tray_animations')}
            />
            <ToggleRow
              label="Allow Canvas"
              description="Allow the agent to show and control the Canvas panel."
              checked={settings.allow_canvas}
              onChange={() => handleToggleSetting('allow_canvas')}
            />
            <ToggleRow
              label="Allow Camera"
              description="Allow the agent to capture a photo or short video via the built-in camera."
              checked={settings.allow_camera}
              onChange={() => handleToggleSetting('allow_camera')}
            />
            <ToggleRow
              label="Enable Peekaboo Bridge"
              description="Allow signed tools to drive UI automation via PeekaBoobridge."
              checked={settings.enable_peekaboo_bridge}
              onChange={() => handleToggleSetting('enable_peekaboo_bridge')}
            />
          </div>
        </div>
      )}
    </div>
  );
}
