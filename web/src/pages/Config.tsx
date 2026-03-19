import { useState, useEffect } from 'react';
import { Settings, Save, CheckCircle, AlertTriangle, ShieldAlert } from 'lucide-react';
import { getConfig, putConfig } from '@/lib/api';
import { t } from '@/lib/i18n';

export default function Config() {
  const [config, setConfig] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  useEffect(() => {
    getConfig()
      .then((data) => { setConfig(typeof data === 'string' ? data : JSON.stringify(data, null, 2)); })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try { await putConfig(config); setSuccess(t('config.save_success')); }
    catch (err: unknown) { setError(err instanceof Error ? err.message : t('config.save_error')); }
    finally { setSaving(false); }
  };

  useEffect(() => {
    if (!success) return;
    const timer = setTimeout(() => setSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [success]);

  if (loading) return (
    <div className="flex items-center justify-center h-64">
      <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
    </div>
  );

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Settings className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>{t('config.configuration_title')}</h2>
        </div>
        <button onClick={handleSave} disabled={saving} className="btn-electric flex items-center gap-2 text-sm px-4 py-2">
          <Save className="h-4 w-4" />{saving ? t('config.saving') : t('config.save')}
        </button>
      </div>

      <div className="flex items-start gap-3 rounded-2xl p-4 border" style={{ borderColor: 'rgba(255, 170, 0, 0.2)', background: 'rgba(255, 170, 0, 0.05)' }}>
        <ShieldAlert className="h-5 w-5 flex-shrink-0 mt-0.5" style={{ color: 'var(--color-status-warning)' }} />
        <div>
          <p className="text-sm font-medium" style={{ color: 'var(--color-status-warning)' }}>{t('config.sensitive_title')}</p>
          <p className="text-sm mt-0.5" style={{ color: 'rgba(255, 170, 0, 0.7)' }}>{t('config.sensitive_hint')}</p>
        </div>
      </div>

      {success && (
        <div className="flex items-center gap-2 rounded-xl p-3 border animate-fade-in" style={{ borderColor: 'rgba(0, 230, 138, 0.2)', background: 'rgba(0, 230, 138, 0.06)' }}>
          <CheckCircle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-success)' }} />
          <span className="text-sm" style={{ color: 'var(--color-status-success)' }}>{success}</span>
        </div>
      )}

      {error && (
        <div className="flex items-center gap-2 rounded-xl p-3 border animate-fade-in" style={{ borderColor: 'rgba(239, 68, 68, 0.2)', background: 'rgba(239, 68, 68, 0.06)' }}>
          <AlertTriangle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-error)' }} />
          <span className="text-sm" style={{ color: 'var(--color-status-error)' }}>{error}</span>
        </div>
      )}

      <div className="card overflow-hidden rounded-2xl">
        <div className="flex items-center justify-between px-4 py-2.5 border-b" style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-accent-glow)' }}>
          <span className="text-[10px] font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-muted)' }}>{t('config.toml_label')}</span>
          <span className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>{config.split('\n').length} {t('config.lines')}</span>
        </div>
        <textarea
          value={config}
          onChange={(e) => setConfig(e.target.value)}
          spellCheck={false}
          className="w-full min-h-[500px] text-sm p-4 resize-y focus:outline-none font-mono"
          style={{ background: 'var(--pc-bg-base)', color: 'var(--pc-text-secondary)', tabSize: 4 }}
        />
      </div>
    </div>
  );
}
