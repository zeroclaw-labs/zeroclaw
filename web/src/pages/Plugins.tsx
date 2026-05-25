import { useState, useEffect } from 'react';
import {
  Puzzle,
  Check,
  Zap,
  ChevronDown,
  AlertTriangle,
  Box,
  Wrench,
  Wifi,
  Database,
  Eye,
  Sparkles,
} from 'lucide-react';
import type { Plugin, WasmPlugin, WasmPluginsResponse, WasmPluginCapability } from '@/types/api';
import { getPlugins, getWasmPlugins } from '@/lib/api';
import { t } from '@/lib/i18n';

// ---------------------------------------------------------------------------
// Built-in integration helpers
// ---------------------------------------------------------------------------

function statusBadge(status: Plugin['status']) {
  switch (status) {
    case 'Active':
      return {
        icon: Check,
        label: t('plugins.status_active'),
        color: 'var(--color-status-success)',
        border: 'rgba(0, 230, 138, 0.2)',
        bg: 'rgba(0, 230, 138, 0.06)'
      };
    case 'Available':
      return {
        icon: Zap,
        label: t('plugins.status_available'),
        color: 'var(--pc-accent)',
        border: 'var(--pc-accent-dim)',
        bg: 'var(--pc-accent-glow)'
      };
  }
}

// ---------------------------------------------------------------------------
// WASM plugin helpers
// ---------------------------------------------------------------------------

const CAPABILITY_META: Record<WasmPluginCapability, { icon: typeof Wrench; labelKey: string; color: string }> = {
  tool: { icon: Wrench, labelKey: 'plugins.capability_tool', color: '#60a5fa' },
  channel: { icon: Wifi, labelKey: 'plugins.capability_channel', color: '#34d399' },
  memory: { icon: Database, labelKey: 'plugins.capability_memory', color: '#fbbf24' },
  observer: { icon: Eye, labelKey: 'plugins.capability_observer', color: '#a78bfa' },
  skill: { icon: Sparkles, labelKey: 'plugins.capability_skill', color: '#f472b6' },
};

function WasmPluginCard({ plugin }: { plugin: WasmPlugin }) {
  return (
    <div className="card p-5 animate-slide-in-up">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h4 className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
              {plugin.name}
            </h4>
            <span
              className="shrink-0 px-1.5 py-0.5 rounded text-[9px] font-mono font-semibold"
              style={{ background: 'var(--pc-hover)', color: 'var(--pc-text-muted)' }}
            >
              v{plugin.version}
            </span>
          </div>
          {plugin.description && (
            <p className="text-sm mt-1 line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>
              {plugin.description}
            </p>
          )}
        </div>
        <span
          className="shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-[10px] font-semibold border"
          style={
            plugin.loaded
              ? { color: 'var(--color-status-success)', border: 'rgba(0, 230, 138, 0.2)', background: 'rgba(0, 230, 138, 0.06)' }
              : { color: '#f59e0b', border: 'rgba(245, 158, 11, 0.2)', background: 'rgba(245, 158, 11, 0.06)' }
          }
        >
          {plugin.loaded ? <Check className="h-3 w-3" /> : <AlertTriangle className="h-3 w-3" />}
          {plugin.loaded ? t('plugins.wasm_loaded') : t('plugins.wasm_not_loaded')}
        </span>
      </div>

      {/* Capability pills */}
      {plugin.capabilities.length > 0 && (
        <div className="flex flex-wrap gap-1.5 mt-3">
          {plugin.capabilities.map((cap) => {
            const meta = CAPABILITY_META[cap];
            const CapIcon = meta.icon;
            return (
              <span
                key={cap}
                className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-semibold"
                style={{ background: `${meta.color}15`, color: meta.color }}
              >
                <CapIcon className="h-2.5 w-2.5" />
                {t(meta.labelKey)}
              </span>
            );
          })}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main page
// ---------------------------------------------------------------------------

export default function Plugins() {
  // Built-in integrations
  const [plugins, setPlugins] = useState<Plugin[]>([]);
  const [activeCategory, setActiveCategory] = useState<string>('all');
  const [builtinOpen, setBuiltinOpen] = useState(true);

  // WASM plugins
  const [wasmData, setWasmData] = useState<WasmPluginsResponse | null>(null);
  const [wasmUnavailable, setWasmUnavailable] = useState(false);
  const [wasmOpen, setWasmOpen] = useState(true);

  // Global state
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    Promise.allSettled([getPlugins(), getWasmPlugins()])
      .then(([builtinResult, wasmResult]) => {
        if (builtinResult.status === 'fulfilled') {
          setPlugins(builtinResult.value);
        } else {
          setError(builtinResult.reason?.message ?? 'Failed to load plugins');
        }

        if (wasmResult.status === 'fulfilled') {
          if (wasmResult.value === null) {
            setWasmUnavailable(true);
          } else {
            setWasmData(wasmResult.value);
          }
        } else {
          // Non-404 error from WASM fetch — don't block built-in section
          setWasmUnavailable(true);
        }
      })
      .finally(() => setLoading(false));
  }, []);

  const categories = ['all', ...Array.from(new Set(plugins.map((i) => i.category))).sort()];
  const filtered = activeCategory === 'all' ? plugins : plugins.filter((i) => i.category === activeCategory);

  const grouped = filtered.reduce<Record<string, Plugin[]>>((acc, item) => {
    const key = item.category;
    if (!acc[key]) acc[key] = [];
    acc[key].push(item);
    return acc;
  }, {});

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'var(--color-status-error-alpha-08)', borderColor: 'var(--color-status-error-alpha-20)', color: 'var(--color-status-error)' }}>
          {t('plugins.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Built-in Integrations Section */}
      <div>
        <button
          onClick={() => setBuiltinOpen((v) => !v)}
          className="flex items-center gap-2 mb-4 w-full text-left group"
          style={{ background: 'transparent', border: 'none', cursor: 'pointer', padding: 0 }}
          aria-expanded={builtinOpen}
        >
          <Puzzle className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <span className="text-sm font-semibold uppercase tracking-wider flex-1" style={{ color: 'var(--pc-text-primary)' }}>
            {t('plugins.builtin_title')} ({plugins.length})
          </span>
          <ChevronDown
            className="h-4 w-4 opacity-40 group-hover:opacity-100"
            style={{ color: 'var(--pc-text-muted)', transform: builtinOpen ? 'rotate(0deg)' : 'rotate(-90deg)', transition: 'transform 0.2s ease, opacity 0.2s ease' }}
          />
        </button>

        {builtinOpen && (
          <>
            {/* Category Filter Tabs */}
            <div className="flex flex-wrap gap-2 mb-4">
              {categories.map((cat) => (
                <button
                  key={cat}
                  onClick={() => setActiveCategory(cat)}
                  className="px-3.5 py-1.5 rounded-xl text-xs font-semibold transition-all capitalize"
                  style={activeCategory === cat
                    ? { background: 'var(--pc-accent)', color: 'white' }
                    : { color: 'var(--pc-text-muted)', border: '1px solid var(--pc-border)', background: 'transparent' }
                  }
                >
                  {cat}
                </button>
              ))}
            </div>

            {/* Grouped Cards */}
            {Object.keys(grouped).length === 0 ? (
              <div className="card p-8 text-center">
                <Puzzle className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
                <p style={{ color: 'var(--pc-text-muted)' }}>{t('plugins.empty')}</p>
              </div>
            ) : (
              Object.entries(grouped).sort(([a], [b]) => a.localeCompare(b)).map(([category, items]) => (
                <div key={category} className="mb-4">
                  <h3 className="text-[10px] font-semibold uppercase tracking-wider mb-3" style={{ color: 'var(--pc-text-faint)' }}>
                    {category}
                  </h3>
                  <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
                    {items.map((plugin) => {
                      const badge = statusBadge(plugin.status);
                      const BadgeIcon = badge.icon;
                      return (
                        <div key={plugin.name} className="card p-5 animate-slide-in-up">
                          <div className="flex items-start justify-between gap-3">
                            <div className="min-w-0">
                              <h4 className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
                                {plugin.name}
                              </h4>
                              <p className="text-sm mt-1 line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>
                                {plugin.description}
                              </p>
                            </div>
                            <span
                              className="shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-[10px] font-semibold border"
                              style={badge}
                            >
                              <BadgeIcon className="h-3 w-3" />
                              {badge.label}
                            </span>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              ))
            )}
          </>
        )}
      </div>

      {/* WASM Extensions Section */}
      {!wasmUnavailable && (
        <div className="animate-slide-in-up" style={{ animationDelay: '200ms' }}>
          <button
            onClick={() => setWasmOpen((v) => !v)}
            className="flex items-center gap-2 mb-4 w-full text-left group"
            style={{ background: 'transparent', border: 'none', cursor: 'pointer', padding: 0 }}
            aria-expanded={wasmOpen}
          >
            <Box className="h-5 w-5" style={{ color: 'var(--color-status-success)' }} />
            <span className="text-sm font-semibold uppercase tracking-wider flex-1" style={{ color: 'var(--pc-text-primary)' }}>
              {t('plugins.wasm_title')} ({wasmData?.plugins.length ?? 0})
            </span>
            <ChevronDown
              className="h-4 w-4 opacity-40 group-hover:opacity-100"
              style={{ color: 'var(--pc-text-muted)', transform: wasmOpen ? 'rotate(0deg)' : 'rotate(-90deg)', transition: 'transform 0.2s ease, opacity 0.2s ease' }}
            />
          </button>

          {wasmOpen && (
            wasmData && !wasmData.plugins_enabled ? (
              <div className="card p-4 flex items-center gap-3">
                <AlertTriangle className="h-4 w-4 shrink-0" style={{ color: '#f59e0b' }} />
                <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
                  {t('plugins.wasm_not_enabled')}
                </p>
              </div>
            ) : wasmData && wasmData.plugins.length === 0 ? (
              <div className="card p-4 flex items-center gap-3">
                <Puzzle className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-faint)' }} />
                <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
                  {t('plugins.wasm_empty')} {wasmData.plugins_dir && (
                    <span className="font-mono text-xs" style={{ color: 'var(--pc-text-faint)' }}>
                      ({wasmData.plugins_dir})
                    </span>
                  )}
                </p>
              </div>
            ) : wasmData ? (
              <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
                {wasmData.plugins.map((p) => (
                  <WasmPluginCard key={p.name} plugin={p} />
                ))}
              </div>
            ) : null
          )}
        </div>
      )}

      {/* WASM not available banner */}
      {wasmUnavailable && (
        <div className="card p-4 flex items-center gap-3 animate-slide-in-up" style={{ animationDelay: '200ms' }}>
          <Box className="h-4 w-4 shrink-0" style={{ color: 'var(--pc-text-faint)' }} />
          <p className="text-sm" style={{ color: 'var(--pc-text-faint)' }}>
            {t('plugins.wasm_unavailable')}
          </p>
        </div>
      )}
    </div>
  );
}
