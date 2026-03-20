import { useState, useEffect, useCallback } from 'react';
import { Smartphone, Trash2 } from 'lucide-react';
import { getAdminPairCode } from '../lib/api';
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

  const token = localStorage.getItem('zeroclaw_token') || '';

  const fetchDevices = useCallback(async () => {
    try {
      const res = await fetch('/api/devices', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setDevices(data.devices || []);
      }
    } catch (err) {
      setError('Failed to load devices');
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
        setError('Failed to generate pairing code');
      }
    } catch (err) {
      setError('Failed to generate pairing code');
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
      }
    } catch (err) {
      setError('Failed to revoke device');
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          {t('pairing.title')}
        </h2>
        <button
          onClick={handleInitiatePairing}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          <Smartphone className="h-4 w-4" />
          {t('pairing.pair_new_device')}
        </button>
      </div>

      {error && (
        <div className="rounded-xl border p-3 text-sm animate-fade-in" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          {error}
        </div>
      )}

      {pairingCode && (
        <div className="card p-6 text-center rounded-2xl">
          <p className="text-xs uppercase tracking-wider mb-2" style={{ color: 'var(--pc-text-muted)' }}>{t('pairing.pairing_code')}</p>
          <div className="text-4xl font-mono font-bold tracking-[0.4em] py-4" style={{ color: 'var(--pc-text-primary)' }}>
            {pairingCode}
          </div>
          <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>{t('pairing.code_hint')}</p>
        </div>
      )}

      <div className="card rounded-2xl overflow-hidden">
        <div className="px-5 py-4 border-b" style={{ borderColor: 'var(--pc-border)' }}>
          <h3 className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            {t('pairing.paired_devices')} ({devices.length})
          </h3>
        </div>
        {devices.length === 0 ? (
          <div className="p-8 text-center" style={{ color: 'var(--pc-text-muted)' }}>
            {t('pairing.no_devices')}
          </div>
        ) : (
          <table className="table-electric">
            <thead>
              <tr>
                <th>{t('pairing.name')}</th>
                <th>{t('pairing.type')}</th>
                <th>{t('pairing.paired')}</th>
                <th>{t('pairing.last_seen')}</th>
                <th>IP</th>
                <th className="text-right">{t('pairing.actions')}</th>
              </tr>
            </thead>
            <tbody>
              {devices.map((device) => (
                <tr key={device.id}>
                  <td style={{ color: 'var(--pc-text-primary)' }}>{device.name || 'Unnamed'}</td>
                  <td style={{ color: 'var(--pc-text-secondary)' }}>{device.device_type || 'Unknown'}</td>
                  <td className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                    {new Date(device.paired_at).toLocaleDateString()}
                  </td>
                  <td className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                    {new Date(device.last_seen).toLocaleString()}
                  </td>
                  <td className="font-mono text-xs" style={{ color: 'var(--pc-text-secondary)' }}>
                    {device.ip_address || '-'}
                  </td>
                  <td className="text-right">
                    <button
                      onClick={() => handleRevokeDevice(device.id)}
                      className="btn-icon"
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
