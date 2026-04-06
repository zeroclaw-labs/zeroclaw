import { useState, useEffect, useCallback } from 'react';
import {
  Shield,
  RefreshCw,
  Eye,
  Mic,
  Camera,
  Monitor,
  Bell,
  Accessibility,
  Wand2,
  AudioLines,
} from 'lucide-react';
import { isTauri, getPermissionsStatus, requestPermission } from '../lib/tauri';
import type { PermissionStatus } from '../types/api';
import { t } from '@/lib/i18n';

const PERMISSION_ICONS: Record<string, React.ComponentType<{ className?: string }>> = {
  accessibility: Accessibility,
  screen_recording: Monitor,
  camera: Camera,
  microphone: Mic,
  automation: Wand2,
  notifications: Bell,
  speech_recognition: AudioLines,
};

const STATUS_COLORS: Record<string, string> = {
  granted: 'var(--color-status-ok)',
  denied: 'var(--color-status-error)',
  not_determined: 'var(--color-status-warn)',
};

const STATUS_LABELS: Record<string, string> = {
  granted: 'Granted',
  denied: 'Denied',
  not_determined: 'Not Determined',
};

export default function Permissions() {
  const [permissions, setPermissions] = useState<PermissionStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [requesting, setRequesting] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!isTauri()) return;
    setLoading(true);
    try {
      const perms = await getPermissionsStatus();
      setPermissions(perms);
    } catch {
      // May fail if not in Tauri context.
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const handleRequest = async (name: string) => {
    setRequesting(name);
    try {
      await requestPermission(name);
      // Re-check after a brief delay (system dialog may take time).
      setTimeout(refresh, 1500);
    } finally {
      setRequesting(null);
    }
  };

  if (!isTauri()) {
    return (
      <div className="p-6">
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          Permissions management is only available in the desktop app.
        </p>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Shield className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
          <h1 className="text-xl font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            {t('permissions.title') || 'Permissions'}
          </h1>
        </div>
        <button onClick={refresh} className="btn-electric px-3 py-1.5 text-sm flex items-center gap-1.5">
          <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>

      <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
        ZeroClaw needs these macOS permissions for desktop automation features.
      </p>

      {/* Permission cards */}
      <div className="space-y-3">
        {permissions.map((perm) => {
          const Icon = PERMISSION_ICONS[perm.name] || Eye;
          const color = STATUS_COLORS[perm.status] || 'var(--pc-text-muted)';
          const statusLabel = STATUS_LABELS[perm.status] || perm.status;

          return (
            <div key={perm.name} className="card p-4 flex items-center justify-between">
              <div className="flex items-center gap-4">
                <div
                  className="h-10 w-10 rounded-xl flex items-center justify-center"
                  style={{ background: 'var(--pc-accent-glow)' }}
                >
                  <Icon className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
                </div>
                <div>
                  <div className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                    {perm.label}
                  </div>
                  <div className="flex items-center gap-1.5 mt-0.5">
                    <div
                      className="h-2 w-2 rounded-full"
                      style={{ background: color }}
                    />
                    <span className="text-xs" style={{ color }}>
                      {statusLabel}
                    </span>
                  </div>
                </div>
              </div>
              {perm.status !== 'granted' && (
                <button
                  onClick={() => handleRequest(perm.name)}
                  disabled={requesting === perm.name}
                  className="btn-electric px-3 py-1.5 text-xs"
                >
                  {requesting === perm.name
                    ? 'Requesting...'
                    : perm.status === 'denied'
                      ? 'Open Settings'
                      : 'Request'}
                </button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
