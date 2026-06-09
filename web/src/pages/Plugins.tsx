// Plugins tab — list and (stub) manage WASM/Extism plugins.
//
// Lists plugins discovered by the gateway (`GET /api/plugins`). That route only
// exists when the gateway is built with the `plugins-wasm` feature (off in the
// default build), so `getPlugins()` returns `null` on 404 and we render a clear
// "not enabled in this build" state.
//
// Management actions (enable, install, remove) call the lifecycle endpoints,
// which currently return `stub: true` — the request is accepted but not yet
// wired to `PluginHost`. We surface that message verbatim instead of claiming
// the action took effect.

import { useCallback, useEffect, useState } from 'react';
import { Boxes, CheckCircle2, CircleSlash, Folder, Plus, Trash2, Power, Info } from 'lucide-react';
import {
  getPlugins,
  installPlugin,
  removePlugin,
  setPluginsEnabled,
  type PluginsResponse,
  type PluginCapability,
  type PluginActionResponse,
} from '@/lib/api';

const CAPABILITY_LABEL: Record<PluginCapability, string> = {
  tool: 'Tool',
  channel: 'Channel',
  memory: 'Memory',
  observer: 'Observer',
  skill: 'Skill',
};

export default function Plugins() {
  const [data, setData] = useState<PluginsResponse | null>(null);
  const [supported, setSupported] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Management state.
  const [installSource, setInstallSource] = useState('');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ kind: 'stub' | 'error'; message: string } | null>(null);

  const load = useCallback(() => {
    return getPlugins()
      .then((resp) => {
        if (resp === null) setSupported(false);
        else setData(resp);
      })
      .catch((err) => setError(err instanceof Error ? err.message : String(err)));
  }, []);

  useEffect(() => {
    load().finally(() => setLoading(false));
  }, [load]);

  // Run a lifecycle action, surface its (stub) message, then refresh the list.
  const runAction = useCallback(
    async (action: () => Promise<PluginActionResponse>) => {
      setBusy(true);
      setNotice(null);
      try {
        const resp = await action();
        setNotice({ kind: resp.stub ? 'stub' : 'error', message: resp.message });
        await load();
      } catch (err) {
        setNotice({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
      } finally {
        setBusy(false);
      }
    },
    [load],
  );

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          Failed to load plugins: {error}
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center gap-3">
        <Boxes className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
        <div>
          <h1 className="text-lg font-semibold" style={{ color: 'var(--pc-text-primary)' }}>Plugins</h1>
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            WASM/Extism plugins discovered by the gateway.
          </p>
        </div>
      </div>

      {!supported ? (
        <div className="card p-6 text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          Plugin support is not enabled in this build. Rebuild the gateway with the
          <code className="mx-1 px-1.5 py-0.5 rounded font-mono" style={{ background: 'var(--pc-bg-base)' }}>plugins-wasm</code>
          feature to manage plugins here.
        </div>
      ) : data ? (
        <>
          {/* Management is stubbed — be explicit so nobody assumes it took effect. */}
          <div className="rounded-2xl border p-3 flex items-start gap-2 text-xs"
            style={{ background: 'rgba(245, 158, 11, 0.08)', borderColor: 'rgba(245, 158, 11, 0.25)', color: '#fbbf24' }}>
            <Info className="h-4 w-4 shrink-0 mt-0.5" />
            <span>
              Enable, install, and remove are <strong>stubs</strong> — the gateway accepts the request but does not yet
              apply it. Wiring to <code className="font-mono">PluginHost</code> is tracked separately.
            </span>
          </div>

          {/* Action result message. */}
          {notice && (
            <div className="rounded-2xl border p-3 text-sm"
              style={notice.kind === 'error'
                ? { background: 'rgba(239,68,68,0.08)', borderColor: 'rgba(239,68,68,0.2)', color: '#f87171' }
                : { background: 'var(--pc-accent-glow)', borderColor: 'var(--pc-accent-dim)', color: 'var(--pc-accent-light)' }}>
              {notice.message}
            </div>
          )}

          <div className="card p-4 flex items-center justify-between gap-4 flex-wrap">
            <div className="flex items-center gap-2 text-sm">
              {data.plugins_enabled
                ? <CheckCircle2 className="h-4 w-4" style={{ color: '#4ade80' }} />
                : <CircleSlash className="h-4 w-4" style={{ color: 'var(--pc-text-muted)' }} />}
              <span style={{ color: 'var(--pc-text-secondary)' }}>
                {data.plugins_enabled ? 'Plugins enabled' : 'Plugins disabled in config ([plugins].enabled)'}
              </span>
            </div>
            <div className="flex items-center gap-3">
              <button
                type="button"
                disabled={busy}
                onClick={() => runAction(() => setPluginsEnabled(!data.plugins_enabled))}
                className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-semibold border disabled:opacity-50"
                style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-secondary)' }}>
                <Power className="h-3.5 w-3.5" />
                {data.plugins_enabled ? 'Disable' : 'Enable'}
              </button>
              <div className="flex items-center gap-2 text-xs font-mono" style={{ color: 'var(--pc-text-faint)' }}>
                <Folder className="h-3.5 w-3.5" /> {data.plugins_dir}
              </div>
            </div>
          </div>

          {/* Install from source. */}
          <div className="card p-4 space-y-2">
            <div className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
              Install
            </div>
            <div className="flex items-center gap-2 flex-wrap">
              <input
                type="text"
                value={installSource}
                disabled={busy}
                onChange={(e) => setInstallSource(e.target.value)}
                placeholder="path, registry name, or git URL"
                className="flex-1 min-w-0 px-3 py-2 rounded-lg text-sm border bg-transparent disabled:opacity-50"
                style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && installSource.trim() && !busy) {
                    runAction(() => installPlugin(installSource.trim())).then(() => setInstallSource(''));
                  }
                }}
              />
              <button
                type="button"
                disabled={busy || !installSource.trim()}
                onClick={() => runAction(() => installPlugin(installSource.trim())).then(() => setInstallSource(''))}
                className="inline-flex items-center gap-1.5 px-3 py-2 rounded-lg text-sm font-semibold disabled:opacity-50"
                style={{ background: 'var(--pc-accent)', color: '#04121f' }}>
                <Plus className="h-4 w-4" /> Install
              </button>
            </div>
          </div>

          <div>
            <div className="text-sm font-semibold uppercase tracking-wider mb-3" style={{ color: 'var(--pc-text-primary)' }}>
              Discovered ({data.plugins.length})
            </div>
            {data.plugins.length === 0 ? (
              <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
                No plugins found in <span className="font-mono">{data.plugins_dir}</span>.
              </p>
            ) : (
              <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
                {data.plugins.map((p) => (
                  <div key={p.name} className="card p-4 space-y-2">
                    <div className="flex items-start justify-between gap-2">
                      <div className="min-w-0">
                        <h3 className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>{p.name}</h3>
                        <span className="text-xs font-mono" style={{ color: 'var(--pc-text-faint)' }}>v{p.version}</span>
                      </div>
                      <div className="flex items-center gap-1.5 shrink-0">
                        <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-semibold border"
                          style={p.loaded
                            ? { borderColor: 'rgba(34,197,94,0.3)', color: '#4ade80', background: 'rgba(34,197,94,0.08)' }
                            : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}>
                          {p.loaded ? 'Loaded' : 'Not loaded'}
                        </span>
                        <button
                          type="button"
                          title={`Remove ${p.name}`}
                          disabled={busy}
                          onClick={() => runAction(() => removePlugin(p.name))}
                          className="p-1 rounded-md disabled:opacity-50"
                          style={{ color: '#f87171' }}>
                          <Trash2 className="h-3.5 w-3.5" />
                        </button>
                      </div>
                    </div>
                    {p.description && (
                      <p className="text-sm line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>{p.description}</p>
                    )}
                    {p.capabilities.length > 0 && (
                      <div className="flex flex-wrap gap-1.5 pt-1">
                        {p.capabilities.map((c) => (
                          <span key={c} className="text-[10px] px-2 py-0.5 rounded-full border"
                            style={{ borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }}>
                            {CAPABILITY_LABEL[c] ?? c}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </>
      ) : null}
    </div>
  );
}
