import { useState, useEffect, useCallback } from 'react';
import { Smartphone, Trash2, X } from 'lucide-react';
import { getAdminPairCode } from '@/lib/api';
import { Button, Card, ConfirmDialog, PageHeader } from '@/components/ui';
import { t } from '@/lib/i18n';

interface Device {
  id: string;
  name: string | null;
  device_type: string | null;
  paired_at: string;
  last_seen: string;
  ip_address: string | null;
}

export default function Pairing() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [pairingCode, setPairingCode] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The device queued for revocation; non-null opens the confirm dialog.
  const [pendingRevoke, setPendingRevoke] = useState<Device | null>(null);
  // True when /api/devices returned 401/403 — this browser isn't paired, so
  // the list can't be read (distinct from an empty registry).
  const [unauthorized, setUnauthorized] = useState(false);

  const token = localStorage.getItem('zeroclaw_token') || '';

  const fetchDevices = useCallback(async () => {
    try {
      const res = await fetch('/api/devices', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setDevices(data.devices || []);
        setUnauthorized(false);
      } else if (res.status === 401 || res.status === 403) {
        // require_pairing is on and this browser isn't paired (no/invalid
        // token), so the gateway rejects the listing. Distinguish this from a
        // genuinely empty registry — otherwise it reads as "0 paired devices"
        // when really we just can't see them.
        setUnauthorized(true);
        setDevices([]);
      } else {
        setError(t('pairing.load_error'));
      }
    } catch (err) {
      setError(t('pairing.load_error'));
    } finally {
      setLoading(false);
    }
  }, [token]);

  // Fetch the current pairing code on mount (if one is active)
  useEffect(() => {
    getAdminPairCode()
      .then((data) => {
        if (data.pairing_code) {
          setPairingCode(data.pairing_code);
        }
      })
      .catch(() => {
        // Admin endpoint not reachable — code will show after clicking "Pair New Device"
      });
  }, []);

  useEffect(() => { fetchDevices(); }, [fetchDevices]);

  const handleInitiatePairing = async () => {
    try {
      const res = await fetch('/api/pairing/initiate', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setPairingCode(data.pairing_code);
      } else {
        setError(t('pairing.generate_error'));
      }
    } catch (err) {
      setError(t('pairing.generate_error'));
    }
  };

  const handleRevokeDevice = async (deviceId: string) => {
    try {
      const res = await fetch(`/api/devices/${deviceId}`, {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        setDevices(devices.filter(d => d.id !== deviceId));
      } else {
        setError(t('pairing.revoke_error'));
      }
    } catch (err) {
      setError(t('pairing.revoke_error'));
    } finally {
      setPendingRevoke(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin border-pc-border border-t-pc-accent" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      <PageHeader
        title={t('pairing.title')}
        actions={
          <Button onClick={handleInitiatePairing}>
            <Smartphone className="h-4 w-4" />
            {t('pairing.pair_new_device')}
          </Button>
        }
      />

      {error && (
        <Card className="flex items-start gap-2 text-sm border-status-error/25 bg-status-error/10 text-status-error">
          <span className="flex-1">{error}</span>
          <button
            type="button"
            onClick={() => setError(null)}
            className="flex-shrink-0 text-status-error/70 hover:text-status-error transition-colors"
            aria-label="Dismiss"
          >
            <X className="h-4 w-4" />
          </button>
        </Card>
      )}

      {pairingCode && (
        <Card className="p-6 text-center">
          <p className="text-xs uppercase tracking-wider mb-2 text-pc-text-muted">
            {t('pairing.pairing_code')}
          </p>
          <div className="text-4xl font-mono font-bold tracking-[0.4em] py-4 text-pc-text">
            {pairingCode}
          </div>
          <p className="text-xs text-pc-text-muted">{t('pairing.code_hint')}</p>
        </Card>
      )}

      <Card padded={false} className="overflow-hidden">
        <div className="px-5 py-4 border-b border-pc-border">
          <h3 className="text-sm font-semibold text-pc-text">
            {t('pairing.paired_devices')}
            {unauthorized ? '' : ` (${devices.length})`}
          </h3>
        </div>
        {unauthorized ? (
          <div className="p-8 text-center text-sm text-pc-text-muted">
            <p className="font-medium text-pc-text-secondary">
              This browser isn’t paired yet
            </p>
            <p className="mt-1">
              Pairing is required, so the device list can’t be read from here.
              Pair this browser with a code (above) — then your paired devices
              will appear.
            </p>
          </div>
        ) : devices.length === 0 ? (
          <div className="p-8 text-center text-sm text-pc-text-muted">
            {t('pairing.no_devices')}
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-pc-border text-left text-xs uppercase tracking-wider text-pc-text-muted">
                  <th className="px-5 py-3 font-medium">{t('pairing.name')}</th>
                  <th className="px-5 py-3 font-medium">{t('pairing.type')}</th>
                  <th className="px-5 py-3 font-medium">{t('pairing.paired')}</th>
                  <th className="px-5 py-3 font-medium">{t('pairing.last_seen')}</th>
                  <th className="px-5 py-3 font-medium">IP</th>
                  <th className="px-5 py-3 font-medium text-right">
                    {t('pairing.actions')}
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-pc-border">
                {devices.map((device) => (
                  <tr
                    key={device.id}
                    className="transition-colors hover:bg-pc-elevated/50"
                  >
                    <td className="px-5 py-3 text-pc-text">
                      {device.name || 'Unnamed'}
                    </td>
                    <td className="px-5 py-3 text-pc-text-secondary">
                      {device.device_type || 'Unknown'}
                    </td>
                    <td className="px-5 py-3 text-xs text-pc-text-muted">
                      {new Date(device.paired_at).toLocaleDateString()}
                    </td>
                    <td className="px-5 py-3 text-xs text-pc-text-muted">
                      {new Date(device.last_seen).toLocaleString()}
                    </td>
                    <td className="px-5 py-3 font-mono text-xs text-pc-text-secondary">
                      {device.ip_address || '-'}
                    </td>
                    <td className="px-5 py-3 text-right">
                      <Button
                        variant="danger"
                        size="sm"
                        onClick={() => setPendingRevoke(device)}
                        aria-label={t('pairing.actions')}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      <ConfirmDialog
        open={pendingRevoke !== null}
        danger
        title="Revoke device?"
        message={
          <>
            This will revoke{' '}
            <span className="text-pc-text-secondary">
              {pendingRevoke?.name || 'this device'}
            </span>
            's access. It will need to be paired again to reconnect.
          </>
        }
        confirmLabel="Revoke"
        onConfirm={() => {
          if (pendingRevoke) void handleRevokeDevice(pendingRevoke.id);
        }}
        onClose={() => setPendingRevoke(null)}
      />
    </div>
  );
}
