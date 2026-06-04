// Dedicated MCP (Model Context Protocol) tab.
//
// Bespoke, transport-aware management for agnostic MCP servers. Server CRUD
// reuses the generic config API: `mcp.servers` is one `object-array` config
// prop, so we read it via `listProps('mcp')` (real env/header values round-trip
// — they are NOT masked for object-array props) and write the whole array back
// via `patchConfig`. Bundles are a map section, edited via map-key + per-field
// patches. The live layer (connection state, discovered tools, on-demand probe)
// comes from the dedicated `/api/mcp/status` + `/api/mcp/test` endpoints.

import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Plug,
  PlugZap,
  Plus,
  Trash2,
  Save,
  CheckCircle2,
  XCircle,
  Loader2,
  ChevronDown,
  ChevronRight,
  Boxes,
} from 'lucide-react';
import {
  listProps,
  patchConfig,
  getMcpStatus,
  testMcpServer,
  putMcpServers,
  createMapKey,
  deleteMapKey,
  type McpServerConfig,
  type McpServerStatus,
  type McpStatusResponse,
  type McpTransport,
  type McpTestResult,
} from '@/lib/api';
import ReloadDaemonButton from '@/components/sections/ReloadDaemonButton';

const TRANSPORTS: McpTransport[] = ['stdio', 'http', 'sse'];
const MAX_TIMEOUT = 600;

// ── Helpers ──────────────────────────────────────────────────────────────

/** Parse a JSON-ish string value from the config list API into a typed array. */
function parseJsonArray<T>(raw: unknown): T[] {
  if (typeof raw !== 'string' || raw.trim() === '' || raw === '<unset>') return [];
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as T[]) : [];
  } catch {
    return [];
  }
}

/** Normalize a raw server object from the config API into our editable shape. */
function normalizeServer(raw: Partial<McpServerConfig>): McpServerConfig {
  return {
    name: raw.name ?? '',
    transport: (raw.transport as McpTransport) ?? 'stdio',
    url: raw.url ?? null,
    command: raw.command ?? '',
    args: Array.isArray(raw.args) ? raw.args : [],
    env: raw.env && typeof raw.env === 'object' ? raw.env : {},
    headers: raw.headers && typeof raw.headers === 'object' ? raw.headers : {},
    tool_timeout_secs: raw.tool_timeout_secs ?? null,
  };
}

interface Bundle {
  alias: string;
  servers: string[];
  exclude: string[];
}

// ── Page ───────────────────────────────────────────────────────────────────

export default function Mcp() {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [enabled, setEnabled] = useState(false);
  const [deferredLoading, setDeferredLoading] = useState(true);
  const [servers, setServers] = useState<McpServerConfig[]>([]);
  const [bundles, setBundles] = useState<Bundle[]>([]);
  const [status, setStatus] = useState<McpStatusResponse | null>(null);

  // True once a config change has been saved this session — surfaces the
  // "reload to apply" banner, since the registry only (re)connects at startup.
  const [pendingReload, setPendingReload] = useState(false);

  const statusByName = useMemo(() => {
    const map = new Map<string, McpServerStatus>();
    status?.servers.forEach((s) => map.set(s.name, s));
    return map;
  }, [status]);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await getMcpStatus());
    } catch {
      /* status is best-effort; the page still works for config editing */
    }
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // `mcp.servers` is a List section with no per-field config-prop paths, so
      // the status endpoint is the source of truth for the server list (and for
      // enabled / deferred_loading). Bundles are a map section, read via props.
      const [st, bundleList] = await Promise.all([
        getMcpStatus(),
        listProps('mcp_bundles').catch(() => ({ entries: [] })),
      ]);

      setStatus(st);
      setEnabled(st.enabled);
      setDeferredLoading(st.deferred_loading);
      setServers(st.servers.map((s) => normalizeServer(s)));

      // Group mcp_bundles.<alias>.{servers,exclude} entries by alias.
      const byAlias = new Map<string, Bundle>();
      for (const e of bundleList.entries) {
        const m = /^mcp_bundles\.([^.]+)\.(servers|exclude)$/.exec(e.path);
        const alias = m?.[1];
        const field = m?.[2];
        if (!alias || (field !== 'servers' && field !== 'exclude')) continue;
        const b = byAlias.get(alias) ?? { alias, servers: [], exclude: [] };
        b[field] = parseJsonArray<string>(e.value);
        byAlias.set(alias, b);
      }
      setBundles([...byAlias.values()].sort((a, b) => a.alias.localeCompare(b.alias)));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // ── Mutations ──────────────────────────────────────────────────────────

  const persistServers = useCallback(
    async (next: McpServerConfig[]) => {
      // Send only the canonical config fields (strip status-only fields like
      // `connected`/`tools` that ride along on the status payload).
      await putMcpServers(next.map(normalizeServer));
      setServers(next);
      setPendingReload(true);
      await refreshStatus();
    },
    [refreshStatus],
  );

  const toggleEnabled = async (value: boolean) => {
    setEnabled(value);
    await patchConfig([{ op: 'replace', path: 'mcp.enabled', value }]);
    setPendingReload(true);
    await refreshStatus();
  };

  const toggleDeferred = async (value: boolean) => {
    setDeferredLoading(value);
    await patchConfig([{ op: 'replace', path: 'mcp.deferred_loading', value }]);
    setPendingReload(true);
  };

  const addServer = () => {
    setServers((prev) => [
      ...prev,
      normalizeServer({ name: '', transport: 'stdio' }),
    ]);
  };

  // ── Render ───────────────────────────────────────────────────────────────

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div
          className="rounded-2xl border p-4"
          style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}
        >
          Failed to load MCP configuration: {error}
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div className="flex items-center gap-3">
          <Plug className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
          <div>
            <h1 className="text-lg font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
              MCP Servers
            </h1>
            <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
              Connect agnostic Model Context Protocol servers and expose their tools to your agents.
            </p>
          </div>
        </div>
      </div>

      {/* Reload-to-apply banner */}
      {pendingReload && (
        <div
          className="rounded-2xl border p-4 flex items-center justify-between gap-4 flex-wrap"
          style={{ background: 'var(--pc-accent-glow)', borderColor: 'var(--pc-accent-dim)' }}
        >
          <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
            Saved. New MCP servers connect only after the daemon reloads — until then{' '}
            <span className="font-medium">Test connection</span> validates a server live without a restart.
          </p>
          <ReloadDaemonButton
            onReloaded={() => {
              setPendingReload(false);
              void load();
            }}
          />
        </div>
      )}

      {/* Global toggles */}
      <div className="card p-4 space-y-3">
        <ToggleRow
          label="Enable MCP"
          description="Load tools from configured MCP servers into the agent."
          checked={enabled}
          onChange={(v) => void toggleEnabled(v)}
        />
        <div className="border-t" style={{ borderColor: 'var(--pc-border)' }} />
        <ToggleRow
          label="Deferred tool loading"
          description="List only tool names in the prompt; fetch full schemas on demand via tool_search (recommended)."
          checked={deferredLoading}
          onChange={(v) => void toggleDeferred(v)}
        />
      </div>

      {/* Servers */}
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <span
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Servers ({servers.length})
          </span>
          <button type="button" onClick={addServer} className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2">
            <Plus className="h-4 w-4" /> Add server
          </button>
        </div>

        {servers.length === 0 ? (
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            No MCP servers configured yet. Click <span className="font-medium">Add server</span> to connect one.
          </p>
        ) : (
          servers.map((server, idx) => (
            <ServerCard
              key={idx}
              server={server}
              status={statusByName.get(server.name)}
              onChange={(next) =>
                setServers((prev) => prev.map((s, i) => (i === idx ? next : s)))
              }
              onSave={() => persistServers(servers)}
              onDelete={() => persistServers(servers.filter((_, i) => i !== idx))}
            />
          ))
        )}
      </div>

      {/* Bundles */}
      <BundlesSection
        bundles={bundles}
        serverNames={servers.map((s) => s.name).filter(Boolean)}
        onChanged={() => {
          setPendingReload(true);
          void load();
        }}
      />
    </div>
  );
}

// ── Sub-components ───────────────────────────────────────────────────────────

function ToggleRow({
  label,
  description,
  checked,
  onChange,
}: {
  label: string;
  description: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="flex items-center justify-between gap-4 cursor-pointer">
      <div>
        <div className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>{label}</div>
        <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>{description}</div>
      </div>
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="h-5 w-9 shrink-0 appearance-none rounded-full transition-colors cursor-pointer"
        style={{ background: checked ? 'var(--pc-accent)' : 'var(--pc-border)' }}
      />
    </label>
  );
}

function StatusBadge({ status }: { status?: McpServerStatus }) {
  if (!status) {
    return (
      <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-semibold border"
        style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}>
        Not connected
      </span>
    );
  }
  if (status.connected) {
    return (
      <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-semibold border"
        style={{ borderColor: 'rgba(34,197,94,0.3)', color: '#4ade80', background: 'rgba(34,197,94,0.08)' }}>
        <CheckCircle2 className="h-3 w-3" /> Connected · {status.tool_count} tool{status.tool_count === 1 ? '' : 's'}
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-semibold border"
      style={{ borderColor: 'rgba(239,68,68,0.3)', color: '#f87171', background: 'rgba(239,68,68,0.08)' }}
      title={status.error ?? undefined}>
      <XCircle className="h-3 w-3" /> Failed
    </span>
  );
}

function ServerCard({
  server,
  status,
  onChange,
  onSave,
  onDelete,
}: {
  server: McpServerConfig;
  status?: McpServerStatus;
  onChange: (next: McpServerConfig) => void;
  onSave: () => Promise<void>;
  onDelete: () => Promise<void>;
}) {
  const [open, setOpen] = useState(true);
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState<'save' | 'delete' | 'test' | null>(null);
  const [test, setTest] = useState<McpTestResult | null>(null);
  const isRemote = server.transport === 'http' || server.transport === 'sse';

  const patch = (p: Partial<McpServerConfig>) => onChange({ ...server, ...p });

  const runSave = async () => {
    if (!server.name.trim()) {
      setTest({ ok: false, error: 'Server name is required before saving.' });
      return;
    }
    setBusy('save');
    setSaving(true);
    try {
      await onSave();
    } finally {
      setSaving(false);
      setBusy(null);
    }
  };

  const runTest = async () => {
    setBusy('test');
    setTest(null);
    try {
      setTest(await testMcpServer(server));
    } catch (e) {
      setTest({ ok: false, error: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(null);
    }
  };

  const runDelete = async () => {
    setBusy('delete');
    try {
      await onDelete();
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="card overflow-hidden">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="w-full text-left p-4 flex items-center justify-between gap-3"
        style={{ background: 'transparent' }}
      >
        <div className="flex items-center gap-2 min-w-0">
          {open ? <ChevronDown className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-accent)' }} />
                : <ChevronRight className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-faint)' }} />}
          <PlugZap className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-accent)' }} />
          <span className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
            {server.name || <em style={{ color: 'var(--pc-text-faint)' }}>unnamed server</em>}
          </span>
          <span className="text-[10px] uppercase tracking-wider px-1.5 py-0.5 rounded"
            style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-muted)' }}>
            {server.transport}
          </span>
        </div>
        <StatusBadge status={status} />
      </button>

      {open && (
        <div className="border-t p-4 space-y-4" style={{ borderColor: 'var(--pc-border)' }}>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <Field label="Name">
              <input
                type="text"
                value={server.name}
                onChange={(e) => patch({ name: e.target.value })}
                placeholder="e.g. filesystem"
                className="input-electric w-full px-3 py-2 text-sm"
              />
            </Field>
            <Field label="Transport">
              <select
                value={server.transport}
                onChange={(e) => patch({ transport: e.target.value as McpTransport })}
                className="input-electric w-full px-3 py-2 text-sm"
              >
                {TRANSPORTS.map((tr) => (
                  <option key={tr} value={tr}>{tr}</option>
                ))}
              </select>
            </Field>
          </div>

          {isRemote ? (
            <>
              <Field label="URL">
                <input
                  type="text"
                  value={server.url ?? ''}
                  onChange={(e) => patch({ url: e.target.value || null })}
                  placeholder="https://mcp.example.com/sse"
                  className="input-electric w-full px-3 py-2 text-sm"
                />
              </Field>
              <KeyValueEditor
                label="Headers (secret)"
                hint="Sent on every request — typically Authorization: Bearer ..."
                entries={server.headers}
                onChange={(headers) => patch({ headers })}
                secret
              />
            </>
          ) : (
            <>
              <Field label="Command">
                <input
                  type="text"
                  value={server.command}
                  onChange={(e) => patch({ command: e.target.value })}
                  placeholder="npx"
                  className="input-electric w-full px-3 py-2 text-sm"
                />
              </Field>
              <Field label="Arguments (one per line)">
                <textarea
                  value={server.args.join('\n')}
                  onChange={(e) =>
                    patch({ args: e.target.value.split('\n').map((s) => s).filter((s, i, a) => !(s === '' && i === a.length - 1)) })
                  }
                  rows={Math.max(2, server.args.length + 1)}
                  placeholder={'-y\n@modelcontextprotocol/server-filesystem\n/tmp'}
                  className="input-electric w-full px-3 py-2 text-sm font-mono"
                />
              </Field>
              <KeyValueEditor
                label="Environment variables (secret)"
                hint="Passed to the spawned process."
                entries={server.env}
                onChange={(env) => patch({ env })}
                secret
              />
            </>
          )}

          <Field label="Tool timeout (seconds, optional)">
            <input
              type="number"
              min={1}
              max={MAX_TIMEOUT}
              value={server.tool_timeout_secs ?? ''}
              onChange={(e) =>
                patch({ tool_timeout_secs: e.target.value === '' ? null : Number(e.target.value) })
              }
              placeholder="180 (default)"
              className="input-electric w-full md:w-48 px-3 py-2 text-sm"
            />
          </Field>

          {/* Discovered tools (from live status) */}
          {status?.connected && status.tools.length > 0 && (
            <div>
              <div className="text-[10px] font-semibold uppercase tracking-wider mb-1.5" style={{ color: 'var(--pc-text-muted)' }}>
                Discovered tools
              </div>
              <div className="flex flex-wrap gap-1.5">
                {status.tools.map((tl) => (
                  <span key={tl.name} className="text-[11px] px-2 py-0.5 rounded-full border font-mono"
                    style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-secondary)' }}
                    title={tl.description ?? undefined}>
                    {tl.name}
                  </span>
                ))}
              </div>
            </div>
          )}

          {/* Test result */}
          {test && (
            <div
              className="rounded-xl border p-3 text-sm"
              style={
                test.ok
                  ? { background: 'rgba(34,197,94,0.08)', borderColor: 'rgba(34,197,94,0.2)', color: '#4ade80' }
                  : { background: 'rgba(239,68,68,0.08)', borderColor: 'rgba(239,68,68,0.2)', color: '#f87171' }
              }
            >
              {test.ok
                ? `Connected — discovered ${test.tool_count ?? 0} tool(s)${test.tools && test.tools.length ? ': ' + test.tools.map((t) => t.name).join(', ') : ''}`
                : `Connection failed: ${test.error ?? 'unknown error'}`}
            </div>
          )}

          {/* Actions */}
          <div className="flex items-center gap-2 flex-wrap">
            <button type="button" onClick={() => void runTest()} disabled={busy !== null}
              className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2">
              {busy === 'test' ? <Loader2 className="h-4 w-4 animate-spin" /> : <PlugZap className="h-4 w-4" />}
              Test connection
            </button>
            <button type="button" onClick={() => void runSave()} disabled={busy !== null}
              className="btn-electric inline-flex items-center gap-1.5 text-sm px-3 py-2">
              {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />}
              Save
            </button>
            <button type="button" onClick={() => void runDelete()} disabled={busy !== null}
              className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2"
              style={{ color: '#f87171' }}>
              {busy === 'delete' ? <Loader2 className="h-4 w-4 animate-spin" /> : <Trash2 className="h-4 w-4" />}
              Delete
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="block text-[11px] font-semibold uppercase tracking-wider mb-1.5" style={{ color: 'var(--pc-text-muted)' }}>
        {label}
      </label>
      {children}
    </div>
  );
}

function KeyValueEditor({
  label,
  hint,
  entries,
  onChange,
  secret,
}: {
  label: string;
  hint?: string;
  entries: Record<string, string>;
  onChange: (next: Record<string, string>) => void;
  secret?: boolean;
}) {
  // Maintain an ordered list of pairs so editing a key doesn't reorder rows.
  const pairs = Object.entries(entries);

  const update = (rows: [string, string][]) => {
    const obj: Record<string, string> = {};
    for (const [k, v] of rows) {
      if (k.trim() === '') continue;
      obj[k] = v;
    }
    onChange(obj);
  };

  return (
    <Field label={label}>
      {hint && <p className="text-xs -mt-1 mb-1.5" style={{ color: 'var(--pc-text-faint)' }}>{hint}</p>}
      <div className="space-y-2">
        {pairs.map(([k, v], i) => (
          <div key={i} className="flex items-center gap-2">
            <input
              type="text"
              value={k}
              onChange={(e) => {
                const next = pairs.map((p, j) => (j === i ? [e.target.value, p[1]] as [string, string] : p));
                update(next);
              }}
              placeholder="KEY"
              className="input-electric flex-1 px-3 py-1.5 text-sm font-mono"
            />
            <input
              type={secret ? 'password' : 'text'}
              value={v}
              onChange={(e) => {
                const next = pairs.map((p, j) => (j === i ? [p[0], e.target.value] as [string, string] : p));
                update(next);
              }}
              placeholder="value"
              className="input-electric flex-1 px-3 py-1.5 text-sm"
            />
            <button
              type="button"
              onClick={() => update(pairs.filter((_, j) => j !== i))}
              className="btn-icon shrink-0"
              aria-label="Remove"
            >
              <Trash2 className="h-4 w-4" style={{ color: '#f87171' }} />
            </button>
          </div>
        ))}
        <button
          type="button"
          onClick={() => update([...pairs, ['', '']])}
          className="btn-secondary inline-flex items-center gap-1.5 text-xs px-2.5 py-1.5"
        >
          <Plus className="h-3.5 w-3.5" /> Add
        </button>
      </div>
    </Field>
  );
}

function BundlesSection({
  bundles,
  serverNames,
  onChanged,
}: {
  bundles: Bundle[];
  serverNames: string[];
  onChanged: () => void;
}) {
  const [newAlias, setNewAlias] = useState('');
  const [adding, setAdding] = useState(false);

  const addBundle = async () => {
    const alias = newAlias.trim();
    if (!alias) return;
    setAdding(true);
    try {
      await createMapKey('mcp_bundles', alias);
      setNewAlias('');
      onChanged();
    } finally {
      setAdding(false);
    }
  };

  return (
    <div className="space-y-4 pt-2">
      <div className="flex items-center gap-2">
        <Boxes className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <span className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          Bundles ({bundles.length})
        </span>
      </div>
      <p className="text-xs -mt-2" style={{ color: 'var(--pc-text-muted)' }}>
        Named groups of MCP servers an agent can reference as one unit.
      </p>

      {bundles.map((b) => (
        <BundleCard key={b.alias} bundle={b} serverNames={serverNames} onChanged={onChanged} />
      ))}

      <div className="flex items-center gap-2">
        <input
          type="text"
          value={newAlias}
          onChange={(e) => setNewAlias(e.target.value)}
          placeholder="new-bundle-name"
          className="input-electric px-3 py-2 text-sm"
        />
        <button type="button" onClick={() => void addBundle()} disabled={adding || !newAlias.trim()}
          className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2">
          {adding ? <Loader2 className="h-4 w-4 animate-spin" /> : <Plus className="h-4 w-4" />} Add bundle
        </button>
      </div>
    </div>
  );
}

function BundleCard({
  bundle,
  serverNames,
  onChanged,
}: {
  bundle: Bundle;
  serverNames: string[];
  onChanged: () => void;
}) {
  const [servers, setServers] = useState<string[]>(bundle.servers);
  const [exclude, setExclude] = useState<string[]>(bundle.exclude);
  const [busy, setBusy] = useState<'save' | 'delete' | null>(null);

  const toggle = (list: string[], setList: (v: string[]) => void, name: string) => {
    setList(list.includes(name) ? list.filter((s) => s !== name) : [...list, name]);
  };

  const save = async () => {
    setBusy('save');
    try {
      await patchConfig([
        { op: 'replace', path: `mcp_bundles.${bundle.alias}.servers`, value: servers },
        { op: 'replace', path: `mcp_bundles.${bundle.alias}.exclude`, value: exclude },
      ]);
      onChanged();
    } finally {
      setBusy(null);
    }
  };

  const remove = async () => {
    setBusy('delete');
    try {
      await deleteMapKey('mcp_bundles', bundle.alias);
      onChanged();
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>{bundle.alias}</span>
        <button type="button" onClick={() => void remove()} disabled={busy !== null}
          className="btn-icon" aria-label="Delete bundle">
          {busy === 'delete' ? <Loader2 className="h-4 w-4 animate-spin" /> : <Trash2 className="h-4 w-4" style={{ color: '#f87171' }} />}
        </button>
      </div>

      <ServerChips label="Include servers" all={serverNames} selected={servers}
        onToggle={(n) => toggle(servers, setServers, n)} />
      <ServerChips label="Exclude servers" all={serverNames} selected={exclude}
        onToggle={(n) => toggle(exclude, setExclude, n)} />

      <button type="button" onClick={() => void save()} disabled={busy !== null}
        className="btn-electric inline-flex items-center gap-1.5 text-sm px-3 py-2">
        {busy === 'save' ? <Loader2 className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />} Save bundle
      </button>
    </div>
  );
}

function ServerChips({
  label,
  all,
  selected,
  onToggle,
}: {
  label: string;
  all: string[];
  selected: string[];
  onToggle: (name: string) => void;
}) {
  return (
    <div>
      <div className="text-[11px] font-semibold uppercase tracking-wider mb-1.5" style={{ color: 'var(--pc-text-muted)' }}>{label}</div>
      {all.length === 0 ? (
        <p className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>Add servers above first.</p>
      ) : (
        <div className="flex flex-wrap gap-1.5">
          {all.map((name) => {
            const on = selected.includes(name);
            return (
              <button
                key={name}
                type="button"
                onClick={() => onToggle(name)}
                className="text-[11px] px-2 py-0.5 rounded-full border font-mono transition-colors"
                style={
                  on
                    ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                    : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }
                }
              >
                {name}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
