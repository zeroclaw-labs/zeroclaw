import { useEffect, useState } from 'react';
import { Smartphone, RefreshCw, ShieldX } from 'lucide-react';
import type { PairedDevice } from '@/types/api';
import { getPairedDevices, revokePairedDevice } from '@/lib/api';

function formatDate(value: string | null): string {
  if (!value) return 'Unknown';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  return date.toLocaleString();
}

export default function Devices() {
  const [devices, setDevices] = useState<PairedDevice[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pendingRevoke, setPendingRevoke] = useState<string | null>(null);

  const loadDevices = async (isRefresh = false) => {
    if (isRefresh) {
      setRefreshing(true);
    } else {
      setLoading(true);
    }
    setError(null);
    try {
      const data = await getPairedDevices();
      setDevices(data);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load paired devices');
    } finally {
      if (isRefresh) {
        setRefreshing(false);
      } else {
        setLoading(false);
      }
    }
  };

  useEffect(() => {
    void loadDevices(false);
  }, []);

  const handleRevoke = async (id: string) => {
    try {
      await revokePairedDevice(id);
      setDevices((prev) => prev.filter((device) => device.id !== id));
      setPendingRevoke(null);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to revoke paired device');
      setPendingRevoke(null);
    }
  };

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Smartphone className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">
            Paired Devices ({devices.length})
          </h2>
        </div>
        <button
          onClick={() => {
            void loadDevices(true);
          }}
          disabled={refreshing}
          className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-60"
        >
          <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
          {error}
        </div>
      )}

      {loading ? (
        <div className="flex h-32 items-center justify-center">
          <div className="h-8 w-8 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
        </div>
      ) : devices.length === 0 ? (
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-8 text-center">
          <ShieldX className="mx-auto mb-3 h-10 w-10 text-gray-600" />
          <p className="text-gray-400">No paired devices found.</p>
        </div>
      ) : (
        <div className="overflow-x-auto rounded-xl border border-gray-800 bg-gray-900">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="px-4 py-3 text-left font-medium text-gray-400">
                  Device ID
                </th>
                <th className="px-4 py-3 text-left font-medium text-gray-400">
                  Paired By
                </th>
                <th className="px-4 py-3 text-left font-medium text-gray-400">
                  Created
                </th>
                <th className="px-4 py-3 text-left font-medium text-gray-400">
                  Last Seen
                </th>
                <th className="px-4 py-3 text-right font-medium text-gray-400">
                  Actions
                </th>
              </tr>
            </thead>
            <tbody>
              {devices.map((device) => (
                <tr
                  key={device.id}
                  className="border-b border-gray-800/50 transition-colors hover:bg-gray-800/30"
                >
                  <td className="px-4 py-3 font-mono text-xs text-white">
                    {device.token_fingerprint}
                  </td>
                  <td className="px-4 py-3 text-gray-300">
                    {device.paired_by ?? 'Unknown'}
                  </td>
                  <td className="px-4 py-3 whitespace-nowrap text-xs text-gray-400">
                    {formatDate(device.created_at)}
                  </td>
                  <td className="px-4 py-3 whitespace-nowrap text-xs text-gray-400">
                    {formatDate(device.last_seen_at)}
                  </td>
                  <td className="px-4 py-3 text-right">
                    {pendingRevoke === device.id ? (
                      <div className="inline-flex items-center gap-2">
                        <span className="text-xs text-red-400">Revoke?</span>
                        <button
                          onClick={() => {
                            void handleRevoke(device.id);
                          }}
                          className="text-xs font-medium text-red-400 hover:text-red-300"
                        >
                          Yes
                        </button>
                        <button
                          onClick={() => setPendingRevoke(null)}
                          className="text-xs font-medium text-gray-400 hover:text-white"
                        >
                          No
                        </button>
                      </div>
                    ) : (
                      <button
                        onClick={() => setPendingRevoke(device.id)}
                        className="text-xs font-medium text-red-400 hover:text-red-300"
                      >
                        Revoke
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
