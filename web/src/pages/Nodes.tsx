import { useCallback, useEffect, useState } from 'react';
import { Network, Server, Cpu, RefreshCw, Trash2 } from 'lucide-react';
import {
  getNodes,
  getPairedDevices,
  revokePairedDevice,
  type DaemonNode,
  type NodePolicy,
  type PairedDeviceInfo,
} from '@/lib/api';

interface UnifiedNode {
  key: string;
  kind: 'daemon' | 'client';
  label: string;
  shortKey: string;
  icon: typeof Network;
  typeLabel: string;
  health: Health;
  lastSeen?: string | null;
  device?: PairedDeviceInfo;
  daemon?: DaemonNode;
  capabilities?: string[];
}

const POLL_INTERVAL_MS = 30_000;
const DEFAULT_POLICY: NodePolicy = { stale_after_secs: 300, offline_after_secs: 1800 };

type Health = 'online' | 'stale' | 'offline';

function deviceHealth(lastSeenIso: string | null | undefined, policy: NodePolicy): Health {
  if (!lastSeenIso) return 'offline';
  try {
    const diffSecs = (Date.now() - new Date(lastSeenIso).getTime()) / 1000;
    if (diffSecs < policy.stale_after_secs) return 'online';
    if (diffSecs < policy.offline_after_secs) return 'stale';
    return 'offline';
  } catch {
    return 'offline';
  }
}

function healthClasses(health: Health) {
  switch (health) {
    case 'online':
      return { cls: 'bg-green-900/20 border-green-700/40 text-green-400', label: 'Online' };
    case 'stale':
      return { cls: 'bg-yellow-900/20 border-yellow-700/40 text-yellow-400', label: 'Stale' };
    case 'offline':
      return { cls: 'bg-red-900/20 border-red-700/40 text-red-400', label: 'Offline' };
  }
}

function formatRelative(iso: string | null | undefined): string {
  if (!iso) return 'Unknown';
  try {
    const diff = Date.now() - new Date(iso).getTime();
    const seconds = Math.floor(diff / 1000);
    if (seconds < 60) return 'just now';
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
  } catch {
    return iso;
  }
}

function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}...${id.slice(-4)}` : id;
}

export default function Nodes() {
  const [devices, setDevices] = useState<PairedDeviceInfo[]>([]);
  const [nodes, setNodes] = useState<DaemonNode[]>([]);
  const [policy, setPolicy] = useState<NodePolicy>(DEFAULT_POLICY);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);

  const fetchAll = useCallback(async () => {
    try {
      const [deviceList, nodeRes] = await Promise.all([getPairedDevices(), getNodes()]);
      setDevices(deviceList);
      setNodes(nodeRes.nodes ?? []);
      setPolicy(nodeRes.policy ?? DEFAULT_POLICY);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchAll();
    const id = window.setInterval(fetchAll, POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, [fetchAll]);

  const handleRevoke = useCallback(
    async (id: string) => {
      if (!window.confirm('Revoke this device? It will need to re-pair.')) return;
      setRevoking(id);
      try {
        await revokePairedDevice(id);
        await fetchAll();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setRevoking(null);
      }
    },
    [fetchAll],
  );

  const unified = unifyNodes(nodes, devices, policy);

  if (loading && devices.length === 0 && nodes.length === 0) {
    return (
      <div className="min-h-[60vh] flex items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-t-transparent" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Network className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
            Nodes ({unified.length})
          </h2>
        </div>
        <button
          onClick={() => fetchAll()}
          className="inline-flex items-center gap-1.5 rounded-xl border px-3 py-1.5 text-xs font-medium transition-all"
          style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}
        >
          <RefreshCw className="h-3.5 w-3.5" /> Refresh
        </button>
      </div>

      <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
        Every ZeroClaw instance across your fleet, with live health.
      </p>

      {error && (
        <div className="rounded-2xl border p-4 text-sm" style={{ background: 'rgba(239,68,68,0.08)', borderColor: 'rgba(239,68,68,0.2)', color: '#f87171' }}>
          {error}
        </div>
      )}

      {unified.length === 0 ? (
        <div className="card p-8 text-center" style={{ color: 'var(--pc-text-muted)' }}>
          <Network className="h-10 w-10 mx-auto mb-3 opacity-30" />
          <p className="text-sm">No nodes connected yet.</p>
          <p className="text-xs mt-1" style={{ color: 'var(--pc-text-faint)' }}>
            Enable <code>[nodes] enabled = true</code> in config and connect nodes via WebSocket.
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
          {unified.map((node) => (
            <NodeCard
              key={node.key}
              node={node}
              revoking={!!node.device && revoking === node.device.id}
              onRevoke={node.device ? () => handleRevoke(node.device!.id) : undefined}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function unifyNodes(
  daemons: DaemonNode[],
  devices: PairedDeviceInfo[],
  policy: NodePolicy,
): UnifiedNode[] {
  const fromDaemons: UnifiedNode[] = daemons.map((d) => ({
    key: `node:${d.node_id}`,
    kind: 'daemon',
    label: d.node_id,
    shortKey: shortId(d.node_id),
    icon: Cpu,
    typeLabel: 'Daemon node',
    health: 'online' as Health,
    capabilities: d.capabilities.map((c) => c.name),
    daemon: d,
  }));

  const fromDevices: UnifiedNode[] = devices.map((d) => ({
    key: `device:${d.id}`,
    kind: 'client',
    label: d.token_fingerprint.slice(0, 12),
    shortKey: shortId(d.id),
    icon: Server,
    typeLabel: 'Paired client',
    health: deviceHealth(d.last_seen_at, policy),
    lastSeen: d.last_seen_at,
    device: d,
  }));

  return [
    ...fromDaemons,
    ...fromDevices.sort((a, b) => {
      const order: Record<Health, number> = { online: 0, stale: 1, offline: 2 };
      return order[a.health] - order[b.health];
    }),
  ];
}

interface NodeCardProps {
  node: UnifiedNode;
  revoking: boolean;
  onRevoke?: () => void;
}

function NodeCard({ node, revoking, onRevoke }: NodeCardProps) {
  const Icon = node.icon;
  const pill = healthClasses(node.health);

  return (
    <div className="card p-5" style={{ opacity: revoking ? 0.5 : 1 }}>
      <div className="flex items-start gap-3">
        <div className="shrink-0 rounded-2xl p-2" style={{ background: 'rgba(var(--pc-accent-rgb), 0.08)', color: 'var(--pc-accent)' }}>
          <Icon className="h-5 w-5" />
        </div>
        <div className="min-w-0 flex-1">
          <h4 className="truncate text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            {node.label}
          </h4>
          <p className="truncate font-mono text-xs" style={{ color: 'var(--pc-text-faint)' }} title={node.key}>
            {node.shortKey}
          </p>
        </div>
        <span className={`shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-medium ${pill.cls}`}>
          {pill.label}
        </span>
        {onRevoke && (
          <button
            onClick={onRevoke}
            disabled={revoking}
            className="shrink-0 rounded-xl p-1.5 transition-all disabled:opacity-50"
            style={{ color: 'var(--color-status-error)' }}
            title="Revoke"
          >
            <Trash2 className="h-4 w-4" />
          </button>
        )}
      </div>

      <div className="mt-4 space-y-1.5 border-t pt-3 text-xs" style={{ borderColor: 'var(--pc-border)' }}>
        <div className="flex justify-between">
          <span style={{ color: 'var(--pc-text-muted)' }}>Type</span>
          <span style={{ color: 'var(--pc-text-secondary)' }}>{node.typeLabel}</span>
        </div>
        {node.lastSeen && (
          <div className="flex justify-between">
            <span style={{ color: 'var(--pc-text-muted)' }}>Last seen</span>
            <span style={{ color: 'var(--pc-text-secondary)' }}>{formatRelative(node.lastSeen)}</span>
          </div>
        )}
        {node.capabilities && node.capabilities.length > 0 && (
          <div className="pt-1">
            <span style={{ color: 'var(--pc-text-muted)' }}>Capabilities ({node.capabilities.length})</span>
            <div className="mt-1.5 flex flex-wrap gap-1">
              {node.capabilities.slice(0, 4).map((cap) => (
                <span
                  key={cap}
                  className="inline-flex items-center rounded-full border px-2 py-0.5 font-mono text-[10px]"
                  style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'var(--pc-bg-base)' }}
                >
                  {cap}
                </span>
              ))}
              {node.capabilities.length > 4 && (
                <span className="self-center text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>
                  +{node.capabilities.length - 4} more
                </span>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
