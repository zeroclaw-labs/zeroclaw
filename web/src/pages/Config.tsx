import { useEffect, useState } from 'react'
import { AlertTriangle, CheckCircle, Save, Settings, ShieldAlert, } from 'lucide-react'
import { getConfig, putConfig } from '@/lib/api'

export default function Config() {
  const [config, setConfig] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  useEffect(() => {
    getConfig()
      .then((data) => {
        setConfig(typeof data === 'string' ? data : JSON.stringify(data, null, 2));
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      await putConfig(config);
      setSuccess('Configuration saved successfully.');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save configuration');
    } finally {
      setSaving(false);
    }
  };

  useEffect(() => {
    if (!success) return;
    const timer = setTimeout(() => setSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [success]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--color-glow-blue)', borderTopColor: 'var(--color-accent-blue)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Settings className="h-5 w-5" style={{ color: 'var(--color-accent-blue)' }} />
          <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-primary)' }}>Configuration</h2>
        </div>
        <button
          onClick={handleSave}
          disabled={saving}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          <Save className="h-4 w-4" />
          {saving ? 'Saving...' : 'Save'}
        </button>
      </div>

      <div className="flex items-start gap-3 rounded-xl p-4 border" style={{ borderColor: 'var(--color-status-warning)', backgroundColor: 'var(--color-bg-warning-subtle)' }}>
        <ShieldAlert className="h-5 w-5 flex-shrink-0 mt-0.5" style={{ color: 'var(--color-status-warning)' }} />
        <div>
          <p className="text-sm font-medium" style={{ color: 'var(--color-status-warning)' }}>
            Sensitive fields are masked
          </p>
          <p className="text-sm mt-0.5" style={{ color: 'var(--color-status-warning)', opacity: 0.8 }}>
            API keys, tokens, and passwords are hidden for security. To update a
            masked field, replace the entire masked value with your new value.
          </p>
        </div>
      </div>

      {success && (
        <div className="flex items-center gap-2 rounded-xl p-3 border animate-fade-in" style={{ borderColor: 'var(--color-status-success)', backgroundColor: 'var(--color-bg-success-subtle)' }}>
          <CheckCircle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-success)' }} />
          <span className="text-sm" style={{ color: 'var(--color-status-success)' }}>{success}</span>
        </div>
      )}

      {error && (
        <div className="flex items-center gap-2 rounded-xl p-3 border animate-fade-in" style={{ borderColor: 'var(--color-status-error)', backgroundColor: 'var(--color-bg-error-subtle)' }}>
          <AlertTriangle className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--color-status-error)' }} />
          <span className="text-sm" style={{ color: 'var(--color-status-error)' }}>{error}</span>
        </div>
      )}

      <div className="glass-card overflow-hidden">
        <div className="flex items-center justify-between px-4 py-2.5 border-b" style={{ borderColor: 'var(--color-border-default)', backgroundColor: 'var(--color-bg-secondary)' }}>
          <span className="text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--color-text-muted)' }}>
            TOML Configuration
          </span>
          <span className="text-xs" style={{ color: 'var(--color-text-muted)' }}>
            {config.split('\n').length} lines
          </span>
        </div>
        <textarea
          value={config}
          onChange={(e) => setConfig(e.target.value)}
          spellCheck={false}
          className="w-full min-h-[500px] font-mono text-sm p-4 resize-y focus:outline-none"
          style={{ 
            backgroundColor: 'var(--color-bg-primary)', 
            color: 'var(--color-text-secondary)',
            borderColor: 'var(--color-accent-blue)',
            outline: 'none'
          }}
        />
      </div>
    </div>
  );
}
