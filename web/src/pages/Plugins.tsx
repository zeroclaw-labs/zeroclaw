import { useState, useEffect, useMemo, useCallback } from 'react';
import { Link } from 'react-router-dom';
import { AlertTriangle, Blocks, Check, ChevronRight, Loader2, Plus, RefreshCw, Search, Trash2, Wrench, X } from 'lucide-react';
import type { Plugin } from '@/types/api';
import { getPlugins, enablePlugin, disablePlugin, reloadPlugins, installPlugin, removePlugin } from '@/lib/api';
import { t } from '@/lib/i18n';

export default function Plugins() {
  const [plugins, setPlugins] = useState<Plugin[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [togglingPlugin, setTogglingPlugin] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [isReloading, setIsReloading] = useState(false);
  const [notification, setNotification] = useState<{ type: 'success' | 'error'; message: string } | null>(null);
  const [isInstallModalOpen, setIsInstallModalOpen] = useState(false);
  const [installSource, setInstallSource] = useState('');
  const [isInstalling, setIsInstalling] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  const [removeTarget, setRemoveTarget] = useState<Plugin | null>(null);
  const [isRemoving, setIsRemoving] = useState(false);

  const filteredPlugins = useMemo(() => {
    if (!searchQuery.trim()) return plugins;
    const query = searchQuery.toLowerCase();
    return plugins.filter(
      (p) =>
        p.name.toLowerCase().includes(query) ||
        p.capabilities.some((cap) => cap.toLowerCase().includes(query)),
    );
  }, [plugins, searchQuery]);

  const fetchPlugins = useCallback(() => {
    return getPlugins()
      .then((res) => setPlugins(res.plugins))
      .catch((err) => setError(err.message));
  }, []);

  useEffect(() => {
    fetchPlugins().finally(() => setLoading(false));
  }, [fetchPlugins]);

  const handleReload = async () => {
    setIsReloading(true);
    setNotification(null);
    try {
      const result = await reloadPlugins();
      if (result.ok) {
        await fetchPlugins();
        setNotification({
          type: 'success',
          message: t('plugin.reload_success').replace('{count}', String(result.total ?? 0)),
        });
      } else {
        setNotification({
          type: 'error',
          message: result.error ?? t('plugin.reload_error'),
        });
      }
    } catch (err) {
      setNotification({
        type: 'error',
        message: err instanceof Error ? err.message : t('plugin.reload_error'),
      });
    } finally {
      setIsReloading(false);
      setTimeout(() => setNotification(null), 4000);
    }
  };

  const togglePlugin = async (plugin: Plugin) => {
    setTogglingPlugin(plugin.name);
    try {
      const updated = plugin.status === 'loaded'
        ? await disablePlugin(plugin.name)
        : await enablePlugin(plugin.name);
      setPlugins((prev) =>
        prev.map((p) => (p.name === plugin.name ? { ...p, status: updated.status } : p)),
      );
    } catch {
      // silently ignore — state stays unchanged
    } finally {
      setTogglingPlugin(null);
    }
  };

  const handleInstall = async () => {
    if (!installSource.trim()) return;
    setIsInstalling(true);
    setInstallError(null);
    setNotification(null);
    try {
      const result = await installPlugin(installSource.trim());
      if (result.ok) {
        await fetchPlugins();
        setNotification({
          type: 'success',
          message: t('plugin.install_success').replace('{name}', result.plugin_name ?? ''),
        });
        setIsInstallModalOpen(false);
        setInstallSource('');
        setInstallError(null);
      } else {
        const errorMsg = result.error ?? t('plugin.install_error');
        setInstallError(errorMsg);
      }
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : t('plugin.install_error');
      setInstallError(errorMsg);
    } finally {
      setIsInstalling(false);
    }
  };

  const closeInstallModal = () => {
    if (isInstalling) return;
    setIsInstallModalOpen(false);
    setInstallSource('');
    setInstallError(null);
  };

  const handleRemove = async () => {
    if (!removeTarget || isRemoving) return;
    setIsRemoving(true);
    setNotification(null);
    try {
      const result = await removePlugin(removeTarget.name);
      if (result.ok) {
        setPlugins((prev) => prev.filter((p) => p.name !== removeTarget.name));
        setNotification({
          type: 'success',
          message: t('plugin.remove_success').replace('{name}', removeTarget.name),
        });
        setRemoveTarget(null);
      } else {
        setNotification({
          type: 'error',
          message: result.error ?? t('plugin.remove_error'),
        });
      }
    } catch (err) {
      setNotification({
        type: 'error',
        message: err instanceof Error ? err.message : t('plugin.remove_error'),
      });
    } finally {
      setIsRemoving(false);
      setRemoveTarget(null);
      setTimeout(() => setNotification(null), 4000);
    }
  };

  const closeRemoveModal = () => {
    if (isRemoving) return;
    setRemoveTarget(null);
  };

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          {t('plugin.load_error')}: {error}
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
      {notification && (
        <div
          className="rounded-lg border px-4 py-3 text-sm flex items-center gap-2 animate-fade-in"
          style={notification.type === 'success'
            ? { background: 'rgba(0, 230, 138, 0.08)', borderColor: 'rgba(0, 230, 138, 0.2)', color: '#00e68a' }
            : { background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }
          }
        >
          {notification.type === 'success' ? <Check className="h-4 w-4" /> : null}
          {notification.message}
        </div>
      )}

      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-2">
            <Blocks className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
            <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
              {t('plugin.title')} ({plugins.length})
            </h2>
          </div>
          <button
            onClick={handleReload}
            disabled={isReloading}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium border transition-colors hover:opacity-80 disabled:opacity-50"
            style={{
              background: 'var(--pc-surface)',
              borderColor: 'var(--pc-border)',
              color: 'var(--pc-text-primary)',
            }}
            title={t('plugin.reload')}
          >
            <RefreshCw className={`h-3.5 w-3.5 ${isReloading ? 'animate-spin' : ''}`} />
            {t('plugin.reload')}
          </button>
          <button
            onClick={() => setIsInstallModalOpen(true)}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium border transition-colors hover:opacity-80"
            style={{
              background: 'var(--pc-accent)',
              borderColor: 'var(--pc-accent)',
              color: 'var(--pc-bg)',
            }}
          >
            <Plus className="h-3.5 w-3.5" />
            {t('plugin.install')}
          </button>
        </div>
        {plugins.length > 0 && (
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4" style={{ color: 'var(--pc-text-faint)' }} />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder={t('plugin.search_placeholder')}
              className="pl-9 pr-3 py-2 rounded-lg text-sm border outline-none focus:ring-2"
              style={{
                background: 'var(--pc-surface)',
                borderColor: 'var(--pc-border)',
                color: 'var(--pc-text-primary)',
              }}
            />
          </div>
        )}
      </div>

      {plugins.length === 0 ? (
        <div className="card p-8 text-center">
          <Blocks className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
          <p style={{ color: 'var(--pc-text-muted)' }}>{t('plugin.empty')}</p>
        </div>
      ) : filteredPlugins.length === 0 ? (
        <div className="card p-8 text-center">
          <Search className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
          <p style={{ color: 'var(--pc-text-muted)' }}>{t('plugin.no_results')}</p>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
          {filteredPlugins.map((plugin) => (
            <div key={plugin.name} className="card p-5 animate-slide-in-up">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <Link
                    to={`/plugins/${encodeURIComponent(plugin.name)}`}
                    className="group/link inline-flex items-center gap-1 hover:underline"
                    style={{ color: 'var(--pc-text-primary)' }}
                  >
                    <h4 className="text-sm font-semibold truncate">{plugin.name}</h4>
                    <ChevronRight className="h-3.5 w-3.5 flex-shrink-0 opacity-40 group-hover/link:opacity-100 transition-opacity" />
                  </Link>
                  <span className="text-[10px] mt-0.5 block" style={{ color: 'var(--pc-text-faint)' }}>
                    v{plugin.version}
                  </span>
                  {plugin.description && (
                    <p className="text-sm mt-1 line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>
                      {plugin.description}
                    </p>
                  )}
                </div>
                <span
                  className="flex-shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-[10px] font-semibold border"
                  style={plugin.status === 'loaded'
                    ? { color: 'var(--color-status-success)', borderColor: 'rgba(0, 230, 138, 0.2)', background: 'rgba(0, 230, 138, 0.06)' }
                    : { color: 'var(--pc-text-muted)', borderColor: 'var(--pc-border)', background: 'transparent' }
                  }
                >
                  <Check className="h-3 w-3" />
                  {plugin.status === 'loaded' ? t('plugin.status_loaded') : t('plugin.status_discovered')}
                </span>
              </div>
              <div className="flex items-center justify-between mt-3">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="inline-flex items-center gap-1 text-[11px]" style={{ color: 'var(--pc-text-muted)' }}>
                    <Wrench className="h-3 w-3" />
                    {t('plugin.tool_count').replace('{count}', String(plugin.tools.length))}
                  </span>
                  {plugin.capabilities.map((cap) => (
                    <span
                      key={cap}
                      className="px-2 py-0.5 rounded-full text-[10px] font-medium border"
                      style={{ color: 'var(--pc-accent)', borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)' }}
                    >
                      {cap}
                    </span>
                  ))}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => setRemoveTarget(plugin)}
                    className="inline-flex items-center justify-center h-6 w-6 rounded-lg transition-colors hover:bg-[rgba(239,68,68,0.1)]"
                    style={{ color: 'var(--pc-text-muted)' }}
                    title={t('plugin.remove')}
                    aria-label={t('plugin.remove')}
                  >
                    <Trash2 className="h-3.5 w-3.5 lucide-trash-2" />
                  </button>
                  <button
                    onClick={() => togglePlugin(plugin)}
                    disabled={togglingPlugin === plugin.name}
                    className={`relative inline-flex h-6 w-11 flex-shrink-0 items-center rounded-full transition-colors duration-300 focus:outline-none ${
                      plugin.status === 'loaded'
                        ? 'bg-[#0080ff]'
                        : 'bg-[#1a1a3e]'
                    }`}
                    title={plugin.status === 'loaded' ? t('plugin.disable') : t('plugin.enable')}
                  >
                    <span
                      className={`inline-block h-4 w-4 rounded-full bg-white transition-transform duration-300 ${
                        plugin.status === 'loaded'
                          ? 'translate-x-6'
                          : 'translate-x-1'
                      }`}
                    />
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Install Plugin Modal */}
      {isInstallModalOpen && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label={t('plugin.install_title')}
          className="fixed inset-0 z-50 flex items-center justify-center"
          onClick={closeInstallModal}
        >
          <div className="absolute inset-0" style={{ background: 'rgba(0,0,0,0.6)', backdropFilter: 'blur(8px)' }} />
          <div
            className="relative w-full max-w-md mx-4 rounded-2xl border shadow-2xl animate-fade-in"
            style={{ background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}
            onClick={(e) => e.stopPropagation()}
          >
            {/* Header */}
            <div
              className="flex items-center justify-between px-5 py-4 border-b"
              style={{ borderColor: 'var(--pc-border)' }}
            >
              <div className="flex items-center gap-2">
                <Plus size={18} style={{ color: 'var(--pc-accent-light)' }} />
                <h2 className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
                  {t('plugin.install_title')}
                </h2>
              </div>
              <button
                onClick={closeInstallModal}
                disabled={isInstalling}
                className="h-7 w-7 rounded-lg flex items-center justify-center transition-colors disabled:opacity-50"
                style={{ color: 'var(--pc-text-muted)', background: 'transparent', border: 'none', cursor: 'pointer' }}
                aria-label="Close"
              >
                <X size={16} />
              </button>
            </div>

            {/* Body */}
            <div className="px-5 py-4">
              <label className="block">
                <span className="text-xs font-medium" style={{ color: 'var(--pc-text-secondary)' }}>
                  {t('plugin.install_source_label')}
                </span>
                <input
                  type="text"
                  value={installSource}
                  onChange={(e) => {
                    setInstallSource(e.target.value);
                    if (installError) setInstallError(null);
                  }}
                  placeholder={t('plugin.install_source_placeholder')}
                  disabled={isInstalling}
                  className="w-full mt-1.5 px-3 py-2.5 rounded-lg text-sm border outline-none focus:ring-2 disabled:opacity-50"
                  style={{
                    background: 'var(--pc-surface)',
                    borderColor: installError ? 'rgba(239, 68, 68, 0.5)' : 'var(--pc-border)',
                    color: 'var(--pc-text-primary)',
                  }}
                  autoFocus
                />
                <span className="text-[11px] mt-1 block" style={{ color: 'var(--pc-text-faint)' }}>
                  {t('plugin.install_source_hint')}
                </span>
              </label>
              {installError && (
                <div
                  data-testid="install-error-message"
                  className="mt-3 rounded-lg border px-3 py-2.5 text-sm"
                  style={{
                    background: 'rgba(239, 68, 68, 0.08)',
                    borderColor: 'rgba(239, 68, 68, 0.2)',
                    color: '#f87171',
                  }}
                >
                  {installError}
                </div>
              )}
            </div>

            {/* Footer */}
            <div
              className="flex items-center justify-end gap-2 px-5 py-4 border-t"
              style={{ borderColor: 'var(--pc-border)' }}
            >
              <button
                onClick={closeInstallModal}
                disabled={isInstalling}
                className="px-4 py-2 rounded-lg text-sm font-medium border transition-colors disabled:opacity-50"
                style={{
                  background: 'transparent',
                  borderColor: 'var(--pc-border)',
                  color: 'var(--pc-text-muted)',
                }}
              >
                {t('plugin.install_cancel')}
              </button>
              <button
                onClick={handleInstall}
                disabled={isInstalling || !installSource.trim()}
                className="px-4 py-2 rounded-lg text-sm font-medium border transition-colors disabled:opacity-50 flex items-center gap-2"
                style={{
                  background: 'var(--pc-accent)',
                  borderColor: 'var(--pc-accent)',
                  color: 'var(--pc-bg)',
                }}
              >
                {isInstalling && (
                  <span data-testid="install-progress-indicator">
                    <Loader2 size={14} className="animate-spin" />
                  </span>
                )}
                {isInstalling ? t('plugin.installing') : t('plugin.install_confirm')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Remove Plugin Confirmation Modal */}
      {removeTarget && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label={t('plugin.remove_title')}
          className="fixed inset-0 z-50 flex items-center justify-center"
          onClick={closeRemoveModal}
        >
          <div className="absolute inset-0" style={{ background: 'rgba(0,0,0,0.6)', backdropFilter: 'blur(8px)' }} />
          <div
            className="relative w-full max-w-md mx-4 rounded-2xl border shadow-2xl animate-fade-in"
            style={{ background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}
            onClick={(e) => e.stopPropagation()}
          >
            {/* Header */}
            <div
              className="flex items-center justify-between px-5 py-4 border-b"
              style={{ borderColor: 'var(--pc-border)' }}
            >
              <div className="flex items-center gap-2">
                <AlertTriangle size={18} style={{ color: '#f87171' }} />
                <h2 className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
                  {t('plugin.remove_title')}
                </h2>
              </div>
              <button
                onClick={closeRemoveModal}
                disabled={isRemoving}
                className="h-7 w-7 rounded-lg flex items-center justify-center transition-colors disabled:opacity-50"
                style={{ color: 'var(--pc-text-muted)', background: 'transparent', border: 'none', cursor: 'pointer' }}
                aria-label="Close"
              >
                <X size={16} />
              </button>
            </div>

            {/* Body */}
            <div className="px-5 py-4">
              <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
                {t('plugin.remove_message').replace('{name}', removeTarget.name)}
              </p>
            </div>

            {/* Footer */}
            <div
              className="flex items-center justify-end gap-2 px-5 py-4 border-t"
              style={{ borderColor: 'var(--pc-border)' }}
            >
              <button
                onClick={closeRemoveModal}
                disabled={isRemoving}
                className="px-4 py-2 rounded-lg text-sm font-medium border transition-colors disabled:opacity-50"
                style={{
                  background: 'transparent',
                  borderColor: 'var(--pc-border)',
                  color: 'var(--pc-text-muted)',
                }}
                aria-label={t('plugin.remove_cancel')}
              >
                {t('plugin.remove_cancel')}
              </button>
              <button
                onClick={handleRemove}
                disabled={isRemoving}
                className="px-4 py-2 rounded-lg text-sm font-medium border transition-colors disabled:opacity-50 flex items-center gap-2"
                style={{
                  background: 'rgba(239, 68, 68, 0.9)',
                  borderColor: 'rgba(239, 68, 68, 0.9)',
                  color: 'white',
                }}
                aria-label={t('plugin.remove_confirm')}
              >
                {isRemoving && (
                  <Loader2 size={14} className="animate-spin" />
                )}
                {isRemoving ? t('plugin.removing') : t('plugin.remove_confirm')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
