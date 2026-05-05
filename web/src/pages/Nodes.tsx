import { useCallback, useEffect, useState } from 'react';
import {
  Network,
  Server,
  Laptop,
  Smartphone,
  Monitor,
  Terminal,
  Trash2,
  RefreshCw,
  Cpu,
  Pencil,
  KeyRound,
  Check,
  X,
} from 'lucide-react';
import {
  getDevices,
  getNodes,
  renameDevice,
  revokeDevice,
  rotateDeviceToken,
  type Device,
  type DaemonNode,
  type NodePolicy,
} from '@/lib/api';

/// Unified node entry — daemons and clients both render through the same shape.
interface UnifiedNode {
  /// Stable identifier for React keys + revoke targeting.
  key: string;
  /// Two flavours: a remote daemon (registered via `/ws/nodes`) or a paired client.
  kind: 'daemon' | 'client';
  /// Friendly label.
  label: string;
  /// Short identifier shown under the label (last 12 chars of full id).
  shortKey: string;
  /// Lucide icon for the node card.
  icon: typeof Network;
  /// "macOS", "Linux", "iOS Phone", "Daemon node", etc.
  typeLabel: string;
  /// Health bucket.
  health: Health;
  /// Optional fields rendered when present.
  lastSeen?: string;
  pairedAt?: string;
  ipAddress?: string | null;
  hostname?: string | null;
  os?: string | null;
  agentVersion?: string | null;
  capabilities?: string[];
  /// Source rows for revoke / future actions.
  device?: Device;
  daemon?: DaemonNode;
}

/// Dashboard refresh cadence. Frontend UX preference, not a server policy
/// — server-driven values (stale/offline thresholds) come through
/// `NodePolicy`.
const POLL_INTERVAL_MS = 30_000;

/// Conservative defaults used until the server has answered with its real
/// policy on first load. Real values come from `[nodes].stale_after_secs` /
/// `[nodes].offline_after_secs` and can be tuned without changing the
/// frontend bundle.
const DEFAULT_POLICY: NodePolicy = {
  stale_after_secs: 300,
  offline_after_secs: 1800,
};

type Health = 'online' | 'stale' | 'offline';

function deviceHealth(lastSeenIso: string, policy: NodePolicy): Health {
  try {
    const diffSecs = (Date.now() - new Date(lastSeenIso).getTime()) / 1000;
    if (diffSecs < policy.stale_after_secs) return 'online';
    if (diffSecs < policy.offline_after_secs) return 'stale';
    return 'offline';
  } catch {
    return 'offline';
  }
}

function healthPillStyle(health: Health) {
  switch (health) {
    case 'online':
      return {
        label: 'Online',
        background: 'rgba(0, 230, 138, 0.08)',
        borderColor: 'rgba(0, 230, 138, 0.25)',
        color: '#34d399',
      };
    case 'stale':
      return {
        label: 'Stale',
        background: 'rgba(252, 165, 0, 0.10)',
        borderColor: 'rgba(252, 165, 0, 0.30)',
        color: 'var(--color-status-warning)',
      };
    case 'offline':
      return {
        label: 'Offline',
        background: 'rgba(239, 68, 68, 0.10)',
        borderColor: 'rgba(239, 68, 68, 0.30)',
        color: 'var(--color-status-error)',
      };
  }
}

function formatRelative(iso: string): string {
  try {
    const diff = Date.now() - new Date(iso).getTime();
    const seconds = Math.floor(diff / 1000);
    if (seconds < 60) return 'just now';
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days} day${days === 1 ? '' : 's'} ago`;
  } catch {
    return iso;
  }
}

function formatAbsolute(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

function deviceIcon(deviceType: string | null) {
  const t = (deviceType ?? '').toLowerCase();
  if (t.includes('phone') || t.includes('ios') || t.includes('android'))
    return Smartphone;
  if (t.includes('laptop') || t.includes('mac') || t.includes('darwin'))
    return Laptop;
  if (t.includes('cli') || t.includes('terminal')) return Terminal;
  if (t.includes('desktop') || t.includes('windows')) return Monitor;
  return Server;
}

function deviceTypeLabel(deviceType: string | null): string {
  if (!deviceType) return 'Unknown';
  return deviceType
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
}

function formatOs(
  name: string | null | undefined,
  version: string | null | undefined,
): string | null {
  if (!name) return null;
  return version ? `${name} ${version}` : name;
}

// ─────────────────────────────────────────────────────────────────

export default function Nodes() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [nodes, setNodes] = useState<DaemonNode[]>([]);
  const [policy, setPolicy] = useState<NodePolicy>(DEFAULT_POLICY);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);

  const fetchAll = useCallback(async () => {
    try {
      const [deviceList, nodeList] = await Promise.all([
        getDevices(),
        getNodes(),
      ]);
      setDevices(deviceList.devices);
      setNodes(nodeList.nodes);
      // Both endpoints return the same policy; prefer the devices response
      // since it's the authoritative one for client lifecycle.
      setPolicy(deviceList.policy ?? nodeList.policy ?? DEFAULT_POLICY);
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
    async (id: string, label: string) => {
      // eslint-disable-next-line no-alert
      if (!window.confirm(`Revoke "${label}"? It will need to re-pair.`)) {
        return;
      }
      setRevoking(id);
      try {
        await revokeDevice(id);
        await fetchAll();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setRevoking(null);
      }
    },
    [fetchAll],
  );

  const handleRename = useCallback(
    async (id: string, newName: string | null) => {
      try {
        await renameDevice(id, newName);
        await fetchAll();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [fetchAll],
  );

  const handleRotate = useCallback(async (id: string, label: string) => {
    try {
      const { pairing_code } = await rotateDeviceToken(id);
      // eslint-disable-next-line no-alert
      window.alert(
        `New pairing code for "${label}": ${pairing_code}\n\nGive this code to the device — it must re-pair within the next few minutes. The old token stays valid until then.`,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  if (loading && devices.length === 0 && nodes.length === 0) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{
            borderColor: 'var(--pc-border)',
            borderTopColor: 'var(--pc-accent)',
          }}
        />
      </div>
    );
  }

  const unified = unifyNodes(nodes, devices, policy);

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center gap-2">
        <Network className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <h2
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          Nodes ({unified.length})
        </h2>
        <button
          type="button"
          onClick={fetchAll}
          className="ml-auto inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-medium border transition-all"
          style={{
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-muted)',
          }}
          aria-label="Refresh"
        >
          <RefreshCw className="h-3.5 w-3.5" /> Refresh
        </button>
      </div>

      <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
        Every ZeroClaw instance across your fleet, with live health.
      </p>

      {error && (
        <div
          className="rounded-2xl border p-4 text-sm"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {error}
        </div>
      )}

      {unified.length === 0 ? (
        <div
          className="card p-8 text-center"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          <Network
            className="h-10 w-10 mx-auto mb-3 opacity-30"
            style={{ color: 'var(--pc-text-faint)' }}
          />
          <p className="text-sm">No nodes connected yet.</p>
          <p
            className="text-xs mt-1"
            style={{ color: 'var(--pc-text-faint)' }}
          >
            Pair a client from this machine, or run{' '}
            <code>zeroclaw node add &lt;url&gt;</code> from another to join it
            to the fleet.
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
          {unified.map((node) => (
            <NodeCard
              key={node.key}
              node={node}
              revoking={!!node.device && revoking === node.device.id}
              onRevoke={
                node.device
                  ? () => handleRevoke(node.device!.id, node.label)
                  : undefined
              }
              onRename={
                node.device
                  ? (newName) => handleRename(node.device!.id, newName)
                  : undefined
              }
              onRotateToken={
                node.device
                  ? () => handleRotate(node.device!.id, node.label)
                  : undefined
              }
            />
          ))}
        </div>
      )}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────

function unifyNodes(
  daemons: DaemonNode[],
  devices: Device[],
  policy: NodePolicy,
): UnifiedNode[] {
  const fromDaemons: UnifiedNode[] = daemons.map((d) => ({
    key: `node:${d.node_id}`,
    kind: 'daemon',
    label: d.node_id,
    shortKey: shortId(d.node_id),
    icon: Cpu,
    typeLabel: 'Daemon node',
    health: 'online',
    capabilities: d.capabilities.map((c) => c.name),
    daemon: d,
  }));

  const fromDevices: UnifiedNode[] = devices.map((d) => ({
    key: `device:${d.id}`,
    kind: 'client',
    label: d.hostname ?? d.name ?? deviceTypeLabel(d.device_type),
    shortKey: shortId(d.id),
    icon: deviceIcon(d.device_type),
    typeLabel: deviceTypeLabel(d.device_type),
    health: deviceHealth(d.last_seen, policy),
    lastSeen: d.last_seen,
    pairedAt: d.paired_at,
    ipAddress: d.ip_address,
    hostname: d.hostname,
    os: formatOs(d.os_name, d.os_version),
    agentVersion: d.agent_version,
    device: d,
  }));

  // Daemon nodes first (they're typically the more important ones), then clients
  // sorted by health (online → stale → offline) and most-recently-seen first.
  return [...fromDaemons, ...fromDevices.sort((a, b) => {
    const order: Record<Health, number> = { online: 0, stale: 1, offline: 2 };
    const ha = order[a.health] - order[b.health];
    if (ha !== 0) return ha;
    if (a.lastSeen && b.lastSeen) {
      return new Date(b.lastSeen).getTime() - new Date(a.lastSeen).getTime();
    }
    return 0;
  })];
}

// ─────────────────────────────────────────────────────────────────

interface NodeCardProps {
  node: UnifiedNode;
  revoking: boolean;
  /// Only paired clients can be revoked; daemons get `undefined` (no button).
  onRevoke?: () => void;
  /// Inline rename. `null` clears the label back to the inferred default.
  onRename?: (newName: string | null) => void;
  /// Issue a fresh pairing code for this device (alerts the user with it).
  onRotateToken?: () => void;
}

function NodeCard({
  node,
  revoking,
  onRevoke,
  onRename,
  onRotateToken,
}: NodeCardProps) {
  const Icon = node.icon;
  const pill = healthPillStyle(node.health);
  const [editing, setEditing] = useState(false);
  const [draftName, setDraftName] = useState(node.label);

  const startEdit = () => {
    setDraftName(node.label);
    setEditing(true);
  };

  const saveEdit = () => {
    const trimmed = draftName.trim();
    onRename?.(trimmed === '' ? null : trimmed);
    setEditing(false);
  };

  const cancelEdit = () => {
    setDraftName(node.label);
    setEditing(false);
  };

  return (
    <div
      className="card p-5 animate-slide-in-up"
      style={{ opacity: revoking ? 0.5 : 1 }}
    >
      <div className="flex items-start gap-3">
        <div
          className="p-2 rounded-2xl shrink-0"
          style={{
            background: 'rgba(var(--pc-accent-rgb), 0.08)',
            color: 'var(--pc-accent)',
          }}
        >
          <Icon className="h-5 w-5" />
        </div>
        <div className="flex-1 min-w-0">
          {editing ? (
            <div className="flex items-center gap-1">
              <input
                type="text"
                value={draftName}
                onChange={(e) => setDraftName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') saveEdit();
                  if (e.key === 'Escape') cancelEdit();
                }}
                autoFocus
                className="flex-1 min-w-0 input-electric text-sm py-1 px-2"
                placeholder="Device label"
              />
              <button
                type="button"
                onClick={saveEdit}
                className="p-1 rounded-xl hover:bg-(--pc-hover)"
                style={{ color: 'var(--color-status-success)' }}
                aria-label="Save"
              >
                <Check className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={cancelEdit}
                className="p-1 rounded-xl hover:bg-(--pc-hover)"
                style={{ color: 'var(--pc-text-muted)' }}
                aria-label="Cancel"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
          ) : (
            <div className="flex items-center gap-1.5 group">
              <h4
                className="text-sm font-semibold truncate"
                style={{ color: 'var(--pc-text-primary)' }}
              >
                {node.label}
              </h4>
              {onRename && (
                <button
                  type="button"
                  onClick={startEdit}
                  className="p-0.5 rounded-md opacity-0 group-hover:opacity-100 transition-opacity hover:bg-(--pc-hover)"
                  style={{ color: 'var(--pc-text-muted)' }}
                  aria-label="Rename"
                  title="Rename"
                >
                  <Pencil className="h-3 w-3" />
                </button>
              )}
            </div>
          )}
          <p
            className="text-xs font-mono truncate"
            style={{ color: 'var(--pc-text-faint)' }}
            title={node.key}
          >
            {node.shortKey}
          </p>
        </div>
        <span
          className="rounded-full px-2 py-0.5 text-[10px] font-medium border shrink-0"
          style={pill}
        >
          {pill.label}
        </span>
        {onRotateToken && (
          <button
            type="button"
            onClick={onRotateToken}
            className="p-1.5 rounded-xl transition-all hover:bg-(--pc-hover)"
            style={{ color: 'var(--pc-text-muted)' }}
            aria-label={`Rotate token for ${node.label}`}
            title="Rotate token"
          >
            <KeyRound className="h-4 w-4" />
          </button>
        )}
        {onRevoke && (
          <button
            type="button"
            onClick={onRevoke}
            disabled={revoking}
            className="p-1.5 rounded-xl transition-all disabled:opacity-50 hover:bg-(--pc-hover)"
            style={{ color: 'var(--color-status-error)' }}
            aria-label={`Revoke ${node.label}`}
            title="Revoke node"
          >
            <Trash2 className="h-4 w-4" />
          </button>
        )}
      </div>

      <div
        className="mt-4 pt-3 border-t space-y-1.5 text-xs"
        style={{ borderColor: 'var(--pc-border)' }}
      >
        {/*
          Show OS when we have a precise value (e.g. "macOS 10.15.7"); fall
          back to the coarser device_type ("Cli", "Browser") when the OS is
          unknown. Avoids the duplication where Type=Macos and OS=macOS
          would both render.
        */}
        <div className="flex justify-between">
          <span style={{ color: 'var(--pc-text-muted)' }}>Platform</span>
          <span
            className="capitalize truncate ml-2"
            style={{ color: 'var(--pc-text-secondary)' }}
          >
            {node.os ?? node.typeLabel}
          </span>
        </div>
        {node.hostname && (
          <div className="flex justify-between">
            <span style={{ color: 'var(--pc-text-muted)' }}>Host</span>
            <span
              className="font-mono truncate ml-2"
              style={{ color: 'var(--pc-text-secondary)' }}
              title={node.hostname}
            >
              {node.hostname}
            </span>
          </div>
        )}
        {node.agentVersion && (
          <div className="flex justify-between">
            <span style={{ color: 'var(--pc-text-muted)' }}>Agent</span>
            <span
              className="font-mono"
              style={{ color: 'var(--pc-text-secondary)' }}
            >
              v{node.agentVersion.replace(/^v/, '')}
            </span>
          </div>
        )}
        {node.lastSeen && (
          <div className="flex justify-between">
            <span style={{ color: 'var(--pc-text-muted)' }}>Last seen</span>
            <span
              title={formatAbsolute(node.lastSeen)}
              style={{ color: 'var(--pc-text-secondary)' }}
            >
              {formatRelative(node.lastSeen)}
            </span>
          </div>
        )}
        {node.pairedAt && (
          <div className="flex justify-between">
            <span style={{ color: 'var(--pc-text-muted)' }}>Joined</span>
            <span
              title={formatAbsolute(node.pairedAt)}
              style={{ color: 'var(--pc-text-secondary)' }}
            >
              {formatRelative(node.pairedAt)}
            </span>
          </div>
        )}
        {node.ipAddress && (
          <div className="flex justify-between">
            <span style={{ color: 'var(--pc-text-muted)' }}>IP</span>
            <span
              className="font-mono"
              style={{ color: 'var(--pc-text-faint)' }}
            >
              {node.ipAddress}
            </span>
          </div>
        )}
        {node.capabilities && node.capabilities.length > 0 && (
          <div className="pt-1">
            <span style={{ color: 'var(--pc-text-muted)' }}>
              Capabilities ({node.capabilities.length})
            </span>
            <div className="flex flex-wrap gap-1 mt-1.5">
              {node.capabilities.slice(0, 4).map((cap) => (
                <span
                  key={cap}
                  className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-mono border"
                  style={{
                    borderColor: 'var(--pc-border)',
                    color: 'var(--pc-text-muted)',
                    background: 'var(--pc-bg-base)',
                  }}
                >
                  {cap}
                </span>
              ))}
              {node.capabilities.length > 4 && (
                <span
                  className="text-[10px] self-center"
                  style={{ color: 'var(--pc-text-faint)' }}
                >
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
