import { useState, useEffect } from 'react';
import { useParams, Link } from 'react-router-dom';
import { ArrowLeft, Package, Shield, Globe, FolderOpen, Settings, Wrench, Lock, CheckCircle, XCircle, ShieldCheck, Pencil, Save, X } from 'lucide-react';
import type { Plugin, PluginToolDef } from '@/types/api';
import { getPlugin, patchPluginConfig } from '@/lib/api';
import { t } from '@/lib/i18n';

function riskBadge(level: PluginToolDef['risk_level']) {
  switch (level) {
    case 'low':
      return { color: 'var(--color-status-success)', bg: 'rgba(0, 230, 138, 0.08)', border: 'rgba(0, 230, 138, 0.2)' };
    case 'medium':
      return { color: 'var(--color-status-warning, #f59e0b)', bg: 'rgba(245, 158, 11, 0.08)', border: 'rgba(245, 158, 11, 0.2)' };
    case 'high':
      return { color: 'var(--color-status-error)', bg: 'rgba(239, 68, 68, 0.08)', border: 'rgba(239, 68, 68, 0.2)' };
  }
}

function ConfigSection({ plugin, onConfigSaved }: { plugin: Plugin; onConfigSaved: (p: Plugin) => void }) {
  const configKeys = Object.keys(plugin.config);
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [editValue, setEditValue] = useState('');
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  function startEdit(key: string, currentValue: string) {
    setEditingKey(key);
    setEditValue(currentValue);
    setSaveError(null);
  }

  function cancelEdit() {
    setEditingKey(null);
    setEditValue('');
    setSaveError(null);
  }

  async function saveEdit(key: string) {
    setSaving(true);
    setSaveError(null);
    try {
      await patchPluginConfig(plugin.name, { [key]: editValue });
      // Update local state to reflect the change
      const updatedConfig = { ...plugin.config };
      const decl = updatedConfig[key];
      if (decl !== null && typeof decl === 'object' && !Array.isArray(decl)) {
        updatedConfig[key] = { ...decl, value: editValue, default: editValue };
      } else {
        updatedConfig[key] = editValue;
      }
      onConfigSaved({ ...plugin, config: updatedConfig });
      setEditingKey(null);
      setEditValue('');
    } catch {
      setSaveError(t('plugin.config_save_error'));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="card p-5">
      <div className="flex items-center gap-2 mb-4">
        <Settings className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
        <h3 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          {t('plugin.config')}
        </h3>
      </div>
      <div className="space-y-2">
        {configKeys.map((key) => {
          const decl = plugin.config[key];
          const isObject = decl !== null && typeof decl === 'object' && !Array.isArray(decl);
          const isRequired = isObject && decl.required === true;
          const isSensitive = isObject && decl.sensitive === true;
          const description: string | undefined = isObject ? decl.description : undefined;
          const hasDefault = isObject ? ('default' in decl) : typeof decl === 'string';
          const isSet = hasDefault || (isObject && decl.value !== undefined);
          const currentValue = isObject
            ? (decl.value ?? decl.default ?? '')
            : (typeof decl === 'string' ? decl : '');
          const isEditing = editingKey === key;

          return (
            <div key={key} className="py-2" style={{ borderBottom: '1px solid var(--pc-border)' }}>
              <div className="flex items-center justify-between gap-3">
                <div className="flex items-center gap-2 min-w-0">
                  {isSensitive ? (
                    <Lock className="h-3.5 w-3.5 flex-shrink-0" style={{ color: 'var(--pc-text-faint)' }} />
                  ) : isSet ? (
                    <CheckCircle className="h-3.5 w-3.5 flex-shrink-0" style={{ color: 'var(--color-status-success)' }} />
                  ) : (
                    <XCircle className="h-3.5 w-3.5 flex-shrink-0" style={{ color: 'var(--color-status-error)' }} />
                  )}
                  <div className="min-w-0">
                    <span className="font-mono text-xs font-medium" style={{ color: 'var(--pc-text-primary)' }}>{key}</span>
                    {description && (
                      <p className="text-[11px] mt-0.5 truncate" style={{ color: 'var(--pc-text-muted)' }}>{description}</p>
                    )}
                  </div>
                </div>
                <div className="flex items-center gap-1.5 flex-shrink-0">
                  {isSensitive && (
                    <span
                      className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border"
                      style={{ color: 'var(--pc-text-muted)', background: 'rgba(128, 128, 128, 0.06)', borderColor: 'var(--pc-border)' }}
                    >
                      {t('plugin.config_sensitive')}
                    </span>
                  )}
                  <span
                    className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border"
                    style={isRequired
                      ? { color: 'var(--color-status-warning, #f59e0b)', background: 'rgba(245, 158, 11, 0.06)', borderColor: 'rgba(245, 158, 11, 0.2)' }
                      : { color: 'var(--pc-text-faint)', background: 'transparent', borderColor: 'var(--pc-border)' }
                    }
                  >
                    {isRequired ? t('plugin.config_required') : t('plugin.config_optional')}
                  </span>
                  <span
                    className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border"
                    style={isSet
                      ? { color: 'var(--color-status-success)', background: 'rgba(0, 230, 138, 0.06)', borderColor: 'rgba(0, 230, 138, 0.2)' }
                      : { color: 'var(--color-status-error)', background: 'rgba(239, 68, 68, 0.06)', borderColor: 'rgba(239, 68, 68, 0.2)' }
                    }
                  >
                    {isSet ? t('plugin.config_set') : t('plugin.config_missing')}
                  </span>
                  {!isSensitive && !isEditing && (
                    <button
                      onClick={() => startEdit(key, String(currentValue))}
                      className="inline-flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-semibold border cursor-pointer"
                      style={{ color: 'var(--pc-accent)', background: 'transparent', borderColor: 'var(--pc-border)' }}
                      title={t('plugin.config_edit')}
                    >
                      <Pencil className="h-3 w-3" />
                      {t('plugin.config_edit')}
                    </button>
                  )}
                </div>
              </div>

              {/* Sensitive: masked value display */}
              {isSensitive && isSet && (
                <div className="mt-1.5 ml-5.5 pl-0.5">
                  <span className="font-mono text-[11px]" style={{ color: 'var(--pc-text-faint)' }}>
                    {t('plugin.config_masked')}
                  </span>
                </div>
              )}

              {/* Inline editor for non-sensitive keys */}
              {isEditing && (
                <div className="mt-2 ml-5.5 pl-0.5 flex items-center gap-2">
                  <input
                    type="text"
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') saveEdit(key);
                      if (e.key === 'Escape') cancelEdit();
                    }}
                    autoFocus
                    className="flex-1 px-2 py-1 rounded text-xs font-mono border outline-none"
                    style={{
                      background: 'var(--pc-bg-base)',
                      borderColor: saveError ? 'rgba(239, 68, 68, 0.5)' : 'var(--pc-border)',
                      color: 'var(--pc-text-primary)',
                    }}
                  />
                  <button
                    onClick={() => saveEdit(key)}
                    disabled={saving}
                    className="inline-flex items-center gap-1 px-2 py-1 rounded text-[10px] font-semibold border cursor-pointer"
                    style={{ color: 'var(--color-status-success)', background: 'rgba(0, 230, 138, 0.06)', borderColor: 'rgba(0, 230, 138, 0.2)' }}
                  >
                    <Save className="h-3 w-3" />
                    {saving ? t('plugin.config_saving') : t('plugin.config_save')}
                  </button>
                  <button
                    onClick={cancelEdit}
                    disabled={saving}
                    className="inline-flex items-center gap-1 px-2 py-1 rounded text-[10px] font-semibold border cursor-pointer"
                    style={{ color: 'var(--pc-text-muted)', background: 'transparent', borderColor: 'var(--pc-border)' }}
                  >
                    <X className="h-3 w-3" />
                    {t('plugin.config_cancel')}
                  </button>
                  {saveError && (
                    <span className="text-[10px]" style={{ color: 'var(--color-status-error)' }}>{saveError}</span>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default function PluginDetail() {
  const { name } = useParams<{ name: string }>();
  const [plugin, setPlugin] = useState<Plugin | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!name) return;
    setLoading(true);
    getPlugin(name)
      .then((p) => {
        if (!p) {
          setError(t('plugin.not_found'));
        } else {
          setPlugin(p);
        }
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, [name]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  if (error || !plugin) {
    return (
      <div className="p-6 animate-fade-in">
        <Link to="/integrations" className="inline-flex items-center gap-1.5 text-xs mb-4 hover:underline" style={{ color: 'var(--pc-accent)' }}>
          <ArrowLeft className="h-3.5 w-3.5" />
          {t('plugin.back')}
        </Link>
        <div className="rounded-2xl border p-4" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          {error || t('plugin.not_found')}
        </div>
      </div>
    );
  }

  const configKeys = Object.keys(plugin.config);
  const hasHosts = plugin.allowed_hosts.length > 0;
  const hasPaths = Object.keys(plugin.allowed_paths).length > 0;

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Back link */}
      <Link to="/integrations" className="inline-flex items-center gap-1.5 text-xs hover:underline" style={{ color: 'var(--pc-accent)' }}>
        <ArrowLeft className="h-3.5 w-3.5" />
        {t('plugin.back')}
      </Link>

      {/* Header */}
      <div className="card p-5">
        <div className="flex items-center justify-between gap-4">
          <div className="flex items-center gap-3 min-w-0">
            <Package className="h-6 w-6 flex-shrink-0" style={{ color: 'var(--pc-accent)' }} />
            <div className="min-w-0">
              <h2 className="text-lg font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
                {plugin.name}
              </h2>
              <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                v{plugin.version}
              </p>
            </div>
          </div>
          <span
            className="flex-shrink-0 inline-flex items-center px-3 py-1.5 rounded-full text-xs font-semibold border"
            style={plugin.status === 'loaded'
              ? { color: 'var(--color-status-success)', background: 'rgba(0, 230, 138, 0.06)', borderColor: 'rgba(0, 230, 138, 0.2)' }
              : { color: 'var(--pc-text-muted)', background: 'transparent', borderColor: 'var(--pc-border)' }
            }
          >
            {plugin.status === 'loaded' ? t('plugin.status_loaded') : t('plugin.status_discovered')}
          </span>
        </div>
        {plugin.description && (
          <p className="text-sm mt-3" style={{ color: 'var(--pc-text-muted)' }}>
            {plugin.description}
          </p>
        )}
      </div>

      {/* Tools */}
      <div className="card p-5">
        <div className="flex items-center gap-2 mb-4">
          <Wrench className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
          <h3 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
            {t('plugin.tools')} ({plugin.tools.length})
          </h3>
        </div>
        {plugin.tools.length === 0 ? (
          <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>{t('plugin.no_tools')}</p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr style={{ borderBottom: '1px solid var(--pc-border)' }}>
                  <th className="text-left py-2 pr-4 text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-faint)' }}>{t('plugin.tool_name')}</th>
                  <th className="text-left py-2 pr-4 text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-faint)' }}>{t('plugin.tool_description')}</th>
                  <th className="text-left py-2 pr-4 text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-faint)' }}>{t('plugin.tool_risk')}</th>
                  <th className="text-left py-2 text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-faint)' }}>{t('plugin.tool_parameters')}</th>
                </tr>
              </thead>
              <tbody>
                {plugin.tools.map((tool) => {
                  const badge = riskBadge(tool.risk_level);
                  return (
                    <tr key={tool.name} style={{ borderBottom: '1px solid var(--pc-border)' }}>
                      <td className="py-2.5 pr-4 font-mono text-xs font-medium" style={{ color: 'var(--pc-text-primary)' }}>{tool.name}</td>
                      <td className="py-2.5 pr-4 text-xs" style={{ color: 'var(--pc-text-muted)' }}>{tool.description}</td>
                      <td className="py-2.5 pr-4">
                        <span
                          className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-semibold border"
                          style={{ color: badge.color, background: badge.bg, borderColor: badge.border }}
                        >
                          {tool.risk_level}
                        </span>
                      </td>
                      <td className="py-2.5">
                        {tool.parameters_schema ? (
                          <pre className="text-[10px] font-mono rounded-lg p-2 overflow-x-auto max-w-xs" style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-muted)' }}>
                            {JSON.stringify(tool.parameters_schema, null, 2)}
                          </pre>
                        ) : (
                          <span className="text-xs" style={{ color: 'var(--pc-text-faint)' }}>—</span>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* Capabilities — Allowed Hosts */}
      {hasHosts && (
        <div className="card p-5">
          <div className="flex items-center gap-2 mb-4">
            <Globe className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
            <h3 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
              {t('plugin.allowed_hosts')}
            </h3>
          </div>
          <ul className="space-y-1.5">
            {plugin.allowed_hosts.map((host) => (
              <li key={host} className="flex items-center gap-2 text-xs font-mono" style={{ color: 'var(--pc-text-muted)' }}>
                <Shield className="h-3.5 w-3.5 flex-shrink-0" style={{ color: 'var(--pc-text-faint)' }} />
                {host}
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* Capabilities — Allowed Paths */}
      {hasPaths && (
        <div className="card p-5">
          <div className="flex items-center gap-2 mb-4">
            <FolderOpen className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
            <h3 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
              {t('plugin.allowed_paths')}
            </h3>
          </div>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr style={{ borderBottom: '1px solid var(--pc-border)' }}>
                  <th className="text-left py-2 pr-4 text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-faint)' }}>{t('plugin.path')}</th>
                  <th className="text-left py-2 text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-faint)' }}>{t('plugin.access')}</th>
                </tr>
              </thead>
              <tbody>
                {Object.entries(plugin.allowed_paths).map(([path, access]) => (
                  <tr key={path} style={{ borderBottom: '1px solid var(--pc-border)' }}>
                    <td className="py-2 pr-4 font-mono text-xs" style={{ color: 'var(--pc-text-primary)' }}>{path}</td>
                    <td className="py-2 text-xs" style={{ color: 'var(--pc-text-muted)' }}>{access}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {/* Configuration */}
      {configKeys.length > 0 && (
        <ConfigSection plugin={plugin} onConfigSaved={(updated) => setPlugin(updated)} />
      )}

      {/* Security Audit */}
      <div className="card p-5">
        <div className="flex items-center gap-2 mb-4">
          <ShieldCheck className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
          <h3 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
            {t('plugin.audit')}
          </h3>
        </div>
        <div className="space-y-4">
          {/* Network Access */}
          <div>
            <h4 className="text-xs font-semibold mb-2" style={{ color: 'var(--pc-text-muted)' }}>
              {t('plugin.audit_network')}
            </h4>
            {hasHosts ? (
              <div className="flex flex-wrap gap-2">
                <span
                  className="inline-flex items-center px-2.5 py-1 rounded-full text-[11px] font-semibold border"
                  style={{ color: 'var(--color-status-warning, #f59e0b)', background: 'rgba(245, 158, 11, 0.08)', borderColor: 'rgba(245, 158, 11, 0.2)' }}
                >
                  <Globe className="h-3 w-3 mr-1.5" />
                  {t('plugin.audit_hosts').replace('{count}', String(plugin.allowed_hosts.length))}
                </span>
                {plugin.allowed_hosts.map((host) => (
                  <span
                    key={host}
                    className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-mono border"
                    style={{ color: 'var(--pc-text-muted)', background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}
                  >
                    {host}
                  </span>
                ))}
              </div>
            ) : (
              <span
                className="inline-flex items-center px-2.5 py-1 rounded-full text-[11px] font-semibold border"
                style={{ color: 'var(--color-status-success)', background: 'rgba(0, 230, 138, 0.08)', borderColor: 'rgba(0, 230, 138, 0.2)' }}
              >
                {t('plugin.audit_no_network')}
              </span>
            )}
          </div>

          {/* Filesystem Access */}
          <div>
            <h4 className="text-xs font-semibold mb-2" style={{ color: 'var(--pc-text-muted)' }}>
              {t('plugin.audit_filesystem')}
            </h4>
            {hasPaths ? (
              <div className="flex flex-wrap gap-2">
                <span
                  className="inline-flex items-center px-2.5 py-1 rounded-full text-[11px] font-semibold border"
                  style={{ color: 'var(--color-status-warning, #f59e0b)', background: 'rgba(245, 158, 11, 0.08)', borderColor: 'rgba(245, 158, 11, 0.2)' }}
                >
                  <FolderOpen className="h-3 w-3 mr-1.5" />
                  {t('plugin.audit_paths').replace('{count}', String(Object.keys(plugin.allowed_paths).length))}
                </span>
                {Object.entries(plugin.allowed_paths).map(([path, access]) => (
                  <span
                    key={path}
                    className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-mono border"
                    style={{ color: 'var(--pc-text-muted)', background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}
                  >
                    {path} ({access})
                  </span>
                ))}
              </div>
            ) : (
              <span
                className="inline-flex items-center px-2.5 py-1 rounded-full text-[11px] font-semibold border"
                style={{ color: 'var(--color-status-success)', background: 'rgba(0, 230, 138, 0.08)', borderColor: 'rgba(0, 230, 138, 0.2)' }}
              >
                {t('plugin.audit_no_filesystem')}
              </span>
            )}
          </div>

          {/* Risk Level Breakdown */}
          {plugin.tools.length > 0 && (() => {
            const counts = { low: 0, medium: 0, high: 0 };
            plugin.tools.forEach((tool) => { counts[tool.risk_level]++; });
            return (
              <div>
                <h4 className="text-xs font-semibold mb-2" style={{ color: 'var(--pc-text-muted)' }}>
                  {t('plugin.audit_risk_breakdown')}
                </h4>
                <div className="flex gap-3">
                  {(['low', 'medium', 'high'] as const).filter((level) => counts[level] > 0).map((level) => {
                    const badge = riskBadge(level);
                    return (
                      <span
                        key={level}
                        className="inline-flex items-center px-2.5 py-1 rounded-full text-[11px] font-semibold border"
                        style={{ color: badge.color, background: badge.bg, borderColor: badge.border }}
                      >
                        {level}: {counts[level]}
                      </span>
                    );
                  })}
                </div>
              </div>
            );
          })()}
        </div>
      </div>
    </div>
  );
}
