import { useState, useEffect, useCallback } from 'react';
import {
  Network,
  RefreshCw,
  RotateCw,
  Save,
  ChevronDown,
} from 'lucide-react';
import { getNodes, getDevices, getConfig, putConfig, revokeDevice, rotateDeviceToken } from '../lib/api';
import type { NodeSummary, DeviceInfo } from '../types/api';
import { t } from '@/lib/i18n';

// ── Helpers ────────────────────────────────────────────────────────────────

function timeAgo(dateStr: string): string {
  const diff = Date.now() - new Date(dateStr).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return 'just now';
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return `${days}d ago`;
}

function truncateId(id: string, len = 16): string {
  return id.length > len ? id.slice(0, len) + '...' : id;
}

// ── Select Component ───────────────────────────────────────────────────────

function Select({
  label,
  description,
  value,
  options,
  onChange,
  rightLabel,
}: {
  label: string;
  description: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (v: string) => void;
  rightLabel?: string;
}) {
  return (
    <div className="flex items-center justify-between py-4 border-t" style={{ borderColor: 'var(--pc-border)' }}>
      <div>
        <div className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>{label}</div>
        <div className="text-xs mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>{description}</div>
      </div>
      <div className="flex items-center gap-3">
        {rightLabel && <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>{rightLabel}</span>}
        <div className="relative">
          <select
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="appearance-none text-sm px-3 py-1.5 pr-8 rounded-lg border"
            style={{
              background: 'var(--pc-bg-surface)',
              borderColor: 'var(--pc-border)',
              color: 'var(--pc-text-primary)',
            }}
          >
            {options.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
          <ChevronDown className="absolute right-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5 pointer-events-none" style={{ color: 'var(--pc-text-muted)' }} />
        </div>
      </div>
    </div>
  );
}

// ── Exec Approvals Section ─────────────────────────────────────────────────

function ExecApprovals() {
  const [target, setTarget] = useState('gateway');
  const [scope, setScope] = useState('defaults');
  const [securityMode, setSecurityMode] = useState('deny');
  const [askMode, setAskMode] = useState('on_miss');
  const [askFallback, setAskFallback] = useState('deny');
  const [autoAllow, setAutoAllow] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    getConfig()
      .then((toml) => {
        // Parse autonomy settings from TOML
        const lines = toml.split('\n');
        let inAutonomy = false;
        for (const line of lines) {
          const trimmed = line.trim();
          if (trimmed === '[autonomy]') { inAutonomy = true; continue; }
          if (trimmed.startsWith('[') && inAutonomy) break;
          if (!inAutonomy) continue;

          const match = trimmed.match(/^(\w+)\s*=\s*"?([^"]*)"?$/);
          if (!match) continue;
          const key = match[1];
          const val = match[2] ?? '';
          if (key === 'level') {
            setSecurityMode(val.toLowerCase() === 'full' ? 'allow' : 'deny');
          }
          if (key === 'require_approval_for_medium_risk') {
            setAskMode(val === 'true' ? 'on_miss' : 'never');
          }
          if (key === 'block_high_risk_commands') {
            setAskFallback(val === 'true' ? 'deny' : 'allow');
          }
        }
        setLoaded(true);
      })
      .catch(() => setLoaded(true));
  }, []);

  const handleSave = async () => {
    setSaving(true);
    try {
      const toml = await getConfig();
      // Update autonomy section values
      let updated = toml;
      const level = securityMode === 'allow' ? 'Full' : 'Supervised';
      const requireApproval = askMode === 'on_miss' ? 'true' : 'false';
      const blockHigh = askFallback === 'deny' ? 'true' : 'false';

      if (updated.includes('level =')) {
        updated = updated.replace(/level\s*=\s*"[^"]*"/, `level = "${level}"`);
      }
      if (updated.includes('require_approval_for_medium_risk')) {
        updated = updated.replace(/require_approval_for_medium_risk\s*=\s*\w+/, `require_approval_for_medium_risk = ${requireApproval}`);
      }
      if (updated.includes('block_high_risk_commands')) {
        updated = updated.replace(/block_high_risk_commands\s*=\s*\w+/, `block_high_risk_commands = ${blockHigh}`);
      }

      await putConfig(updated);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="card p-6">
      <div className="flex items-center justify-between mb-1">
        <div>
          <h2 className="text-base font-semibold" style={{ color: 'var(--pc-text-primary)' }}>Exec approvals</h2>
          <p className="text-xs mt-1" style={{ color: 'var(--pc-text-muted)' }}>
            Allowlist and approval policy for exec <code className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>host={target}/node</code>.
          </p>
        </div>
        <button
          onClick={handleSave}
          disabled={saving || !loaded}
          className="text-sm flex items-center gap-1.5 px-3 py-1.5"
          style={{ color: 'var(--pc-accent)' }}
        >
          {saving ? <RotateCw className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
          Save
        </button>
      </div>

      <Select
        label="Target"
        description="Gateway edits local approvals; node edits the selected node."
        value={target}
        options={[{ value: 'gateway', label: 'Gateway' }, { value: 'node', label: 'Node' }]}
        onChange={setTarget}
        rightLabel="Host"
      />

      {/* Scope tabs */}
      <div className="flex items-center gap-2 py-3 border-t" style={{ borderColor: 'var(--pc-border)' }}>
        <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>Scope</span>
        {['defaults', 'main'].map((s) => (
          <button
            key={s}
            onClick={() => setScope(s)}
            className="text-xs px-3 py-1 rounded-lg capitalize"
            style={{
              background: scope === s ? 'var(--pc-accent)' : 'transparent',
              color: scope === s ? '#fff' : 'var(--pc-text-muted)',
              border: scope === s ? 'none' : '1px solid var(--pc-border)',
            }}
          >
            {s === 'defaults' ? 'Defaults' : s}
          </button>
        ))}
      </div>

      <Select
        label="Security"
        description="Default security mode."
        value={securityMode}
        options={[{ value: 'deny', label: 'Deny' }, { value: 'allow', label: 'Allow' }]}
        onChange={setSecurityMode}
        rightLabel="Mode"
      />

      <Select
        label="Ask"
        description="Default prompt policy."
        value={askMode}
        options={[
          { value: 'on_miss', label: 'On miss' },
          { value: 'always', label: 'Always' },
          { value: 'never', label: 'Never' },
        ]}
        onChange={setAskMode}
        rightLabel="Mode"
      />

      <Select
        label="Ask fallback"
        description="Applied when the UI prompt is unavailable."
        value={askFallback}
        options={[{ value: 'deny', label: 'Deny' }, { value: 'allow', label: 'Allow' }]}
        onChange={setAskFallback}
        rightLabel="Fallback"
      />

      <div className="flex items-center justify-between py-4 border-t" style={{ borderColor: 'var(--pc-border)' }}>
        <div>
          <div className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>Auto-allow skill CLIs</div>
          <div className="text-xs mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>Allow skill executables listed by the Gateway.</div>
        </div>
        <div className="flex items-center gap-3">
          <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>Enabled</span>
          <button
            onClick={() => setAutoAllow(!autoAllow)}
            className="relative inline-flex h-5 w-9 items-center rounded-full transition-colors"
            style={{ background: autoAllow ? 'var(--pc-accent)' : 'var(--pc-border)' }}
            role="switch"
            aria-checked={autoAllow}
          >
            <span
              className="inline-block h-3.5 w-3.5 rounded-full transition-transform bg-white"
              style={{ transform: autoAllow ? 'translateX(17px)' : 'translateX(3px)' }}
            />
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Tokens Section ─────────────────────────────────────────────────────────

function TokensSection({ devices, onRefresh }: { devices: DeviceInfo[]; onRefresh: () => void }) {
  const [rotating, setRotating] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);

  const handleRotate = async (id: string) => {
    setRotating(id);
    try {
      await rotateDeviceToken(id);
      onRefresh();
    } catch (e) {
      console.error('Rotate failed:', e);
    } finally {
      setRotating(null);
    }
  };

  const handleRevoke = async (id: string) => {
    setRevoking(id);
    try {
      await revokeDevice(id);
      onRefresh();
    } catch (e) {
      console.error('Revoke failed:', e);
    } finally {
      setRevoking(null);
    }
  };

  if (devices.length === 0) return null;

  return (
    <div className="card p-6 space-y-4">
      {devices.map((device) => (
        <div key={device.id} className="space-y-3">
          {/* Token hash */}
          <div className="font-mono text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
            {device.id}
          </div>
          <div className="font-mono text-xs" style={{ color: 'var(--pc-text-faint)' }}>
            {device.id}
          </div>

          {/* Roles/scopes */}
          <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
            roles: operator &middot; scopes: operator.admin, operator.read, operator.write, operator.approvals, operator.pairing
          </div>

          {/* Tokens info */}
          <div className="text-xs font-medium mt-3 mb-1" style={{ color: 'var(--pc-text-muted)' }}>Tokens</div>
          <div className="flex items-center justify-between">
            <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              operator &middot; active &middot; scopes: operator.admin, operator.approvals, operator.pairing, operator.read, operator.write &middot; {timeAgo(device.last_seen)}
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={() => handleRotate(device.id)}
                disabled={rotating === device.id}
                className="text-xs px-3 py-1.5 rounded-lg border"
                style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }}
              >
                {rotating === device.id ? 'Rotating...' : 'Rotate'}
              </button>
              <button
                onClick={() => handleRevoke(device.id)}
                disabled={revoking === device.id}
                className="text-xs px-3 py-1.5 rounded-lg"
                style={{ background: 'rgba(239, 68, 68, 0.15)', color: 'var(--color-status-error)' }}
              >
                {revoking === device.id ? 'Revoking...' : 'Revoke'}
              </button>
            </div>
          </div>

          {/* Divider between devices */}
          <div className="border-t pt-3 mt-3" style={{ borderColor: 'var(--pc-border)' }} />
        </div>
      ))}
    </div>
  );
}

// ── Nodes Section (Paired Devices & Live Links) ────────────────────────────

function NodesDevicesSection({
  devices,
  nodes,
  loading,
  onRefresh,
}: {
  devices: DeviceInfo[];
  nodes: NodeSummary[];
  loading: boolean;
  onRefresh: () => void;
}) {
  const connectedIds = new Set(nodes.map((n) => n.node_id));

  // Merge: devices + any connected nodes not in devices list
  const mergedDevices = [...devices];
  for (const node of nodes) {
    if (!devices.some((d) => d.id === node.node_id || d.name === node.node_id)) {
      mergedDevices.push({
        id: node.node_id,
        name: node.node_id,
        device_type: 'node',
        paired_at: new Date().toISOString(),
        last_seen: new Date().toISOString(),
        ip_address: null,
      });
    }
  }

  // Get capabilities for a device by matching node_id
  const getCapabilities = (deviceId: string, deviceName: string | null): string[] => {
    const node = nodes.find((n) => n.node_id === deviceId || n.node_id === deviceName);
    return node ? node.capabilities.map((c) => c.name) : [];
  };

  return (
    <div className="card p-6">
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-base font-semibold" style={{ color: 'var(--pc-text-primary)' }}>Nodes</h2>
          <p className="text-xs mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>Paired devices and live links.</p>
        </div>
        <button
          onClick={onRefresh}
          className="text-sm flex items-center gap-1.5 px-3 py-1.5 rounded-lg border"
          style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }}
        >
          <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>

      {mergedDevices.length === 0 ? (
        <div className="py-8 text-center">
          <Network className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>No paired devices or connected nodes.</p>
        </div>
      ) : (
        <div className="space-y-5">
          {mergedDevices.map((device) => {
            const isConnected = connectedIds.has(device.id) || connectedIds.has(device.name ?? '');
            const caps = getCapabilities(device.id, device.name);

            return (
              <div key={device.id} className="border-t pt-4" style={{ borderColor: 'var(--pc-border)' }}>
                {/* Device name + ID */}
                <div className="flex items-start justify-between">
                  <div>
                    <div className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
                      {device.name || device.id}
                    </div>
                    <div className="font-mono text-xs mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>
                      {truncateId(device.id, 48)}
                    </div>
                  </div>
                </div>

                {/* Status badges */}
                <div className="flex flex-wrap items-center gap-2 mt-2.5">
                  <span
                    className="text-xs px-2.5 py-0.5 rounded-full"
                    style={{ background: 'rgba(var(--pc-accent-rgb), 0.1)', color: 'var(--pc-text-muted)' }}
                  >
                    paired
                  </span>
                  {isConnected && (
                    <span
                      className="text-xs px-2.5 py-0.5 rounded-full font-medium"
                      style={{ background: 'rgba(34, 197, 94, 0.15)', color: 'rgb(34, 197, 94)' }}
                    >
                      connected
                    </span>
                  )}
                  {/* Capability tags */}
                  {caps.map((cap) => {
                    // Show short name (last part after last dot)
                    const short = cap.includes('.') ? cap : cap;
                    return (
                      <span
                        key={cap}
                        className="text-xs px-2.5 py-0.5 rounded-full"
                        style={{ background: 'var(--pc-hover)', color: 'var(--pc-text-muted)' }}
                      >
                        {short}
                      </span>
                    );
                  })}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

// ── Main Page ──────────────────────────────────────────────────────────────

export default function Nodes() {
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    setLoading(true);
    setError(null);
    Promise.all([
      getNodes().catch(() => [] as NodeSummary[]),
      getDevices().catch(() => [] as DeviceInfo[]),
    ])
      .then(([n, d]) => {
        setNodes(n);
        setDevices(d);
      })
      .catch((e) => setError(e.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 10000);
    return () => clearInterval(interval);
  }, [refresh]);

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div>
        <h1 className="text-2xl font-bold" style={{ color: 'var(--color-status-error)' }}>
          {t('nodes.title') || 'Nodes'}
        </h1>
        <p className="text-sm mt-1" style={{ color: 'var(--pc-text-muted)' }}>
          Paired devices and commands.
        </p>
      </div>

      {/* Error */}
      {error && (
        <div className="card p-4" style={{ borderColor: 'rgba(239, 68, 68, 0.3)' }}>
          <p className="text-sm" style={{ color: 'var(--color-status-error)' }}>{error}</p>
        </div>
      )}

      {/* Section 1: Exec Approvals */}
      <ExecApprovals />

      {/* Section 2: Tokens */}
      <TokensSection devices={devices} onRefresh={refresh} />

      {/* Section 3: Nodes (devices + connected nodes) */}
      <NodesDevicesSection
        devices={devices}
        nodes={nodes}
        loading={loading}
        onRefresh={refresh}
      />
    </div>
  );
}
