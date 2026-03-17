import { useState, useEffect, useCallback } from 'react';

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

  useEffect(() => {
    fetchDevices();
  }, [fetchDevices]);

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
    return <div className="p-6">Loading...</div>;
  }

  return (
    <div className="p-6 max-w-4xl mx-auto">
      <div className="flex justify-between items-center mb-6">
        <h1 className="text-2xl font-bold">Device Pairing</h1>
        <button
          onClick={handleInitiatePairing}
          className="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700"
        >
          Pair New Device
        </button>
      </div>

      {error && (
        <div className="mb-4 p-3 bg-red-100 text-red-700 rounded">
          {error}
          <button onClick={() => setError(null)} className="ml-2 font-bold">×</button>
        </div>
      )}

      {pairingCode && (
        <div className="mb-6 p-4 bg-blue-50 border border-blue-200 rounded">
          <h2 className="text-lg font-semibold mb-2">Pairing Code</h2>
          <div className="text-3xl font-mono font-bold tracking-wider text-center py-4">
            {pairingCode}
          </div>
          <div className="text-center my-3 text-sm text-gray-400">
            {/* QR code rendering placeholder - will use qrcode.react when available */}
            <div className="inline-block border-2 border-dashed border-gray-300 p-8 rounded">
              <span className="text-gray-400">QR Code</span>
            </div>
          </div>
          <p className="text-sm text-gray-600 text-center">
            Scan the QR code or enter the code manually on the new device.
          </p>
        </div>
      )}

      <div className="bg-white rounded shadow">
        <div className="px-4 py-3 border-b">
          <h2 className="font-semibold">Paired Devices ({devices.length})</h2>
        </div>
        {devices.length === 0 ? (
          <div className="p-4 text-gray-500 text-center">
            No devices paired yet. Click &quot;Pair New Device&quot; to get started.
          </div>
        ) : (
          <table className="w-full">
            <thead>
              <tr className="text-left text-sm text-gray-500 border-b">
                <th className="px-4 py-2">Name</th>
                <th className="px-4 py-2">Type</th>
                <th className="px-4 py-2">Paired</th>
                <th className="px-4 py-2">Last Seen</th>
                <th className="px-4 py-2">IP</th>
                <th className="px-4 py-2">Actions</th>
              </tr>
            </thead>
            <tbody>
              {devices.map(device => (
                <tr key={device.id} className="border-b hover:bg-gray-50">
                  <td className="px-4 py-2">{device.name || 'Unnamed'}</td>
                  <td className="px-4 py-2">{device.device_type || 'Unknown'}</td>
                  <td className="px-4 py-2 text-sm">
                    {new Date(device.paired_at).toLocaleDateString()}
                  </td>
                  <td className="px-4 py-2 text-sm">
                    {new Date(device.last_seen).toLocaleString()}
                  </td>
                  <td className="px-4 py-2 text-sm font-mono">{device.ip_address || '-'}</td>
                  <td className="px-4 py-2">
                    <button
                      onClick={() => handleRevokeDevice(device.id)}
                      className="text-red-600 hover:text-red-800 text-sm"
                    >
                      Revoke
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
