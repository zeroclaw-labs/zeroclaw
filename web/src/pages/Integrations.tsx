import { useState, useEffect, useCallback } from 'react';
import { Puzzle, Check, Zap, Clock, Settings2, X, Save, AlertTriangle, Loader2, Eye, EyeOff } from 'lucide-react';
import type { Integration } from '@/types/api';
import { getIntegrations, getConfig, putConfig } from '@/lib/api';
import { t } from '@/lib/i18n';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ChannelFieldDef {
    key: string;
    label: string;
    type: 'text' | 'password' | 'textarea' | 'toggle' | 'number';
    placeholder?: string;
    help?: string;
    required?: boolean;
}

interface ChannelSchema {
    tomlSection: string;
    label: string;
    fields: ChannelFieldDef[];
}

// ---------------------------------------------------------------------------
// Channel schema registry
// ---------------------------------------------------------------------------

const CHANNEL_SCHEMAS: Record<string, ChannelSchema> = {
    Telegram: {
        tomlSection: 'channels_config.telegram',
        label: 'Telegram',
        fields: [
            { key: 'bot_token', label: 'Bot Token', type: 'password', placeholder: '123456:ABC-DEF...', required: true },
            { key: 'allowed_users', label: 'Allowed User IDs', type: 'textarea', placeholder: 'One per line', help: 'Telegram numeric user IDs, one per line' },
        ],
    },
    Discord: {
        tomlSection: 'channels_config.discord',
        label: 'Discord',
        fields: [
            { key: 'bot_token', label: 'Bot Token', type: 'password', required: true },
            { key: 'allowed_guilds', label: 'Allowed Guild IDs', type: 'textarea', placeholder: 'One per line' },
            { key: 'allowed_users', label: 'Allowed User IDs', type: 'textarea', placeholder: 'One per line' },
        ],
    },
    Slack: {
        tomlSection: 'channels_config.slack',
        label: 'Slack',
        fields: [
            { key: 'bot_token', label: 'Bot Token', type: 'password', placeholder: 'xoxb-...', required: true },
            { key: 'app_token', label: 'App-Level Token', type: 'password', placeholder: 'xapp-...', required: true },
            { key: 'allowed_users', label: 'Allowed User IDs', type: 'textarea', placeholder: 'One per line' },
        ],
    },
    DingTalk: {
        tomlSection: 'channels_config.dingtalk',
        label: 'DingTalk',
        fields: [
            { key: 'client_id', label: 'Client ID', type: 'text', required: true },
            { key: 'client_secret', label: 'Client Secret', type: 'password', required: true },
            { key: 'allowed_users', label: 'Allowed User IDs', type: 'textarea', placeholder: 'One per line' },
        ],
    },
    WhatsApp: {
        tomlSection: 'channels_config.whatsapp',
        label: 'WhatsApp',
        fields: [
            { key: 'phone_number_id', label: 'Phone Number ID', type: 'text' },
            { key: 'access_token', label: 'Access Token', type: 'password' },
            { key: 'session_path', label: 'Session Path (Web mode)', type: 'text', placeholder: '~/.zeroclaw/whatsapp-session' },
            { key: 'allowed_users', label: 'Allowed Users', type: 'textarea', placeholder: 'One per line' },
        ],
    },
    Matrix: {
        tomlSection: 'channels_config.matrix',
        label: 'Matrix',
        fields: [
            { key: 'homeserver', label: 'Homeserver URL', type: 'text', placeholder: 'https://matrix.org', required: true },
            { key: 'username', label: 'Username', type: 'text', required: true },
            { key: 'password', label: 'Password', type: 'password', required: true },
            { key: 'allowed_users', label: 'Allowed User IDs', type: 'textarea', placeholder: 'One per line' },
        ],
    },
    Mattermost: {
        tomlSection: 'channels_config.mattermost',
        label: 'Mattermost',
        fields: [
            { key: 'server_url', label: 'Server URL', type: 'text', placeholder: 'https://mattermost.example.com', required: true },
            { key: 'bot_token', label: 'Bot Token', type: 'password', required: true },
            { key: 'allowed_users', label: 'Allowed User IDs', type: 'textarea', placeholder: 'One per line' },
        ],
    },
    'Lark / Feishu': {
        tomlSection: 'channels_config.lark',
        label: 'Lark / Feishu',
        fields: [
            { key: 'app_id', label: 'App ID', type: 'text', required: true },
            { key: 'app_secret', label: 'App Secret', type: 'password', required: true },
            { key: 'encrypt_key', label: 'Encrypt Key', type: 'password' },
            { key: 'verification_token', label: 'Verification Token', type: 'password' },
            { key: 'allowed_users', label: 'Allowed User IDs', type: 'textarea', placeholder: 'One per line' },
        ],
    },
    Email: {
        tomlSection: 'channels_config.email',
        label: 'Email (IMAP/SMTP)',
        fields: [
            { key: 'imap_host', label: 'IMAP Host', type: 'text', required: true },
            { key: 'imap_port', label: 'IMAP Port', type: 'number', placeholder: '993' },
            { key: 'smtp_host', label: 'SMTP Host', type: 'text', required: true },
            { key: 'smtp_port', label: 'SMTP Port', type: 'number', placeholder: '587' },
            { key: 'username', label: 'Username / Email', type: 'text', required: true },
            { key: 'password', label: 'Password', type: 'password', required: true },
            { key: 'allowed_senders', label: 'Allowed Senders', type: 'textarea', placeholder: 'One email per line' },
        ],
    },
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function statusBadge(status: Integration['status']) {
  switch (status) {
    case 'Active':
      return {
        icon: Check,
        label: t('integrations.status_active'),
        color: 'var(--color-status-success)',
        border: 'rgba(0, 230, 138, 0.2)',
        bg: 'rgba(0, 230, 138, 0.06)'
      };
    case 'Available':
      return {
        icon: Zap,
        label: t('integrations.status_available'),
        color: 'var(--pc-accent)',
        border: 'var(--pc-accent-dim)',
        bg: 'var(--pc-accent-glow)'
      };
    case 'ComingSoon':
      return {
        icon: Clock,
        label: t('integrations.status_coming_soon'),
        color: 'var(--pc-text-muted)',
        border: 'var(--pc-border)',
        bg: 'transparent'
      };
  }
}

function getTomlSectionValues(raw: string, section: string): Record<string, string> {
  const result: Record<string, string> = {};
  const header = `[${section}]`;
  const idx = raw.indexOf(header);
  if (idx === -1) return result;

  const after = raw.slice(idx + header.length);
  const nextSection = after.search(/^\[/m);
  const block = nextSection === -1 ? after : after.slice(0, nextSection);

  for (const line of block.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#') || trimmed.startsWith('[')) continue;
    const eq = trimmed.indexOf('=');
    if (eq === -1) continue;
    const key = trimmed.slice(0, eq).trim();
    let val = trimmed.slice(eq + 1).trim();

    if (val.startsWith('[')) {
      try {
        const arr = JSON.parse(val.replace(/'/g, '"'));
        val = Array.isArray(arr) ? arr.join('\n') : val;
      } catch {
        val = val.replace(/[\[\]"']/g, '').split(',').map((s: string) => s.trim()).join('\n');
      }
    } else {
      val = val.replace(/^["']|["']$/g, '');
    }
    result[key] = val;
  }
  return result;
}

function toTomlValue(val: string, field: ChannelFieldDef): string {
  if (field.type === 'textarea') {
    const items = val.split('\n').map(s => s.trim()).filter(Boolean);
    return `[${items.map(i => `"${i}"`).join(', ')}]`;
  }
  if (field.type === 'number') return val;
  if (field.type === 'toggle') return val === 'true' ? 'true' : 'false';
  return `"${val}"`;
}

function patchTomlSection(
  raw: string,
  section: string,
  values: Record<string, string>,
  fields: ChannelFieldDef[],
): string {
  const header = `[${section}]`;
  const idx = raw.indexOf(header);

  if (idx === -1) {
    const lines: string[] = [];
    for (const field of fields) {
      const val = values[field.key];
      if (val === undefined || val === '' || val === '***MASKED***') continue;
      lines.push(`${field.key} = ${toTomlValue(val, field)}`);
    }
    if (lines.length === 0) return raw;
    return raw.trimEnd() + '\n\n' + header + '\n' + lines.join('\n') + '\n';
  }

  const afterHeader = raw.slice(idx + header.length);
  const nextSectionMatch = afterHeader.search(/^\[/m);
  const sectionBody = nextSectionMatch === -1 ? afterHeader : afterHeader.slice(0, nextSectionMatch);
  const afterSection = nextSectionMatch === -1 ? '' : afterHeader.slice(nextSectionMatch);
  const beforeSection = raw.slice(0, idx);

  const existingLines = sectionBody.split('\n');
  const updatedKeys = new Set<string>();
  const patchedLines: string[] = [];
  const fieldMap = new Map(fields.map(f => [f.key, f]));

  for (const line of existingLines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) {
      patchedLines.push(line);
      continue;
    }

    const eq = trimmed.indexOf('=');
    if (eq === -1) {
      patchedLines.push(line);
      continue;
    }

    const key = trimmed.slice(0, eq).trim();
    const field = fieldMap.get(key);

    if (!field) {
      patchedLines.push(line);
      continue;
    }

    updatedKeys.add(key);
    const newVal = values[key];

    if (newVal === undefined || newVal === '') {
      continue;
    }

    if (newVal === '***MASKED***') {
      patchedLines.push(line);
      continue;
    }

    patchedLines.push(`${key} = ${toTomlValue(newVal, field)}`);
  }

  for (const field of fields) {
    if (updatedKeys.has(field.key)) continue;
    const val = values[field.key];
    if (val === undefined || val === '' || val === '***MASKED***') continue;
    patchedLines.push(`${field.key} = ${toTomlValue(val, field)}`);
  }

  return beforeSection + header + '\n' + patchedLines.join('\n') + (afterSection ? '\n' + afterSection : '\n');
}

// ---------------------------------------------------------------------------
// Configure Modal Component
// ---------------------------------------------------------------------------

interface ConfigureModalProps {
  integration: Integration;
  schema: ChannelSchema;
  onClose: () => void;
  onSaved: () => void;
}

function ConfigureModal({ integration, schema, onClose, onSaved }: ConfigureModalProps) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [rawConfig, setRawConfig] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);
  const [visibleFields, setVisibleFields] = useState<Set<string>>(new Set());

  useEffect(() => {
    getConfig()
      .then((toml: string) => {
        setRawConfig(toml);
        const existing = getTomlSectionValues(toml, schema.tomlSection);
        setValues(existing);
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, [schema.tomlSection]);

  const handleChange = useCallback((key: string, val: string) => {
    setValues((prev) => ({ ...prev, [key]: val }));
    setSuccess(false);
    setError(null);
  }, []);

  const handleSave = useCallback(async () => {
    setSaving(true);
    setError(null);
    setSuccess(false);
    try {
      const patched = patchTomlSection(rawConfig, schema.tomlSection, values, schema.fields);
      await putConfig(patched);
      setSuccess(true);
      setTimeout(() => onSaved(), 800);
    } catch (err: any) {
      setError(err?.message ?? 'Failed to save configuration');
    } finally {
      setSaving(false);
    }
  }, [rawConfig, schema, values, onSaved]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      style={{ background: 'rgba(0,0,0,0.6)', backdropFilter: 'blur(6px)' }}
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div
        className="w-full max-w-lg rounded-2xl border shadow-2xl animate-slide-in-up"
        style={{ background: 'linear-gradient(145deg, #0d0d1a 0%, #111128 100%)', borderColor: 'var(--pc-border)' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-6 py-4" style={{ borderBottom: '1px solid var(--pc-border)' }}>
          <div className="flex items-center gap-3 min-w-0">
            <Settings2 className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
            <div className="min-w-0">
              <h3 className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
                {t('integrations.configure') || 'Configure'} {integration.name}
              </h3>
              <p className="text-[10px] mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>{schema.tomlSection}</p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg transition-colors"
            style={{ color: 'var(--pc-text-muted)', background: 'transparent' }}
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="px-6 py-5 space-y-4 max-h-[60vh] overflow-y-auto custom-scrollbar">
          {loading ? (
            <div className="flex items-center justify-center py-10">
              <Loader2 className="h-6 w-6 animate-spin" style={{ color: 'var(--pc-accent)' }} />
            </div>
          ) : (
            schema.fields.map((field) => (
              <div key={field.key}>
                <label className="flex items-center gap-1.5 text-xs font-semibold mb-1.5" style={{ color: 'var(--pc-text-secondary)' }}>
                  {field.label}
                  {field.required && <span className="text-[#ff4466]">*</span>}
                </label>

                {field.type === 'textarea' ? (
                  <textarea
                    rows={3}
                    className="w-full rounded-xl bg-[#0a0a18] border px-3.5 py-2.5 text-sm placeholder:text-[#334060] focus:outline-none focus:border-[#0080ff40] focus:ring-1 focus:ring-[#0080ff20] transition-all resize-none font-mono"
                    style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }}
                    placeholder={field.placeholder}
                    value={values[field.key] ?? ''}
                    onChange={(e) => handleChange(field.key, e.target.value)}
                  />
                ) : field.type === 'toggle' ? (
                  <button
                    onClick={() => handleChange(field.key, values[field.key] === 'true' ? 'false' : 'true')}
                    className={`relative w-11 h-6 rounded-full transition-colors ${values[field.key] === 'true' ? 'bg-[#0080ff]' : ''}`}
                    style={{ background: values[field.key] === 'true' ? 'var(--pc-accent)' : 'var(--pc-border)' }}
                  >
                    <span
                      className={`absolute top-0.5 left-0.5 h-5 w-5 rounded-full bg-white transition-transform ${values[field.key] === 'true' ? 'translate-x-5' : ''}`}
                    />
                  </button>
                ) : field.type === 'password' ? (
                  <div className="relative">
                    <input
                      type={visibleFields.has(field.key) ? 'text' : 'password'}
                      className="w-full rounded-xl bg-[#0a0a18] border px-3.5 py-2.5 pr-10 text-sm placeholder:text-[#334060] focus:outline-none focus:border-[#0080ff40] focus:ring-1 focus:ring-[#0080ff20] transition-all font-mono"
                      style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }}
                      placeholder={field.placeholder}
                      value={values[field.key] ?? ''}
                      onChange={(e) => handleChange(field.key, e.target.value)}
                    />
                    <button
                      type="button"
                      onClick={() => setVisibleFields((prev) => {
                        const next = new Set(prev);
                        if (next.has(field.key)) next.delete(field.key);
                        else next.add(field.key);
                        return next;
                      })}
                      className="absolute right-2.5 top-1/2 -translate-y-1/2 p-1 rounded-lg transition-colors"
                      style={{ color: 'var(--pc-text-muted)', background: 'transparent' }}
                      tabIndex={-1}
                    >
                      {visibleFields.has(field.key) ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                    </button>
                  </div>
                ) : (
                  <input
                    type={field.type === 'number' ? 'number' : 'text'}
                    className="w-full rounded-xl bg-[#0a0a18] border px-3.5 py-2.5 text-sm placeholder:text-[#334060] focus:outline-none focus:border-[#0080ff40] focus:ring-1 focus:ring-[#0080ff20] transition-all font-mono"
                    style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }}
                    placeholder={field.placeholder}
                    value={values[field.key] ?? ''}
                    onChange={(e) => handleChange(field.key, e.target.value)}
                  />
                )}

                {field.help && (
                  <p className="text-[10px] mt-1" style={{ color: 'var(--pc-text-faint)' }}>{field.help}</p>
                )}
              </div>
            ))
          )}

          {error && (
            <div className="flex items-start gap-2 rounded-xl px-3.5 py-2.5" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)' }}>
              <AlertTriangle className="h-4 w-4 flex-shrink-0 mt-0.5" style={{ color: '#f87171' }} />
              <p className="text-xs" style={{ color: '#f87171' }}>{error}</p>
            </div>
          )}
          {success && (
            <div className="flex items-center gap-2 rounded-xl px-3.5 py-2.5" style={{ background: 'rgba(0, 230, 138, 0.08)', borderColor: 'rgba(0, 230, 138, 0.2)' }}>
              <Check className="h-4 w-4" style={{ color: '#00e68a' }} />
              <p className="text-xs" style={{ color: '#00e68a' }}>{t('integrations.saved') || 'Configuration saved successfully'}</p>
            </div>
          )}
        </div>

        <div className="flex items-center justify-between px-6 py-4" style={{ borderTop: '1px solid var(--pc-border)' }}>
          <p className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>
            {t('integrations.masked_hint') || 'Masked fields (***) will be preserved unless you change them.'}
          </p>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 rounded-xl text-xs font-semibold transition-all"
              style={{ color: 'var(--pc-text-muted)', border: '1px solid var(--pc-border)', background: 'transparent' }}
            >
              {t('common.cancel') || 'Cancel'}
            </button>
            <button
              onClick={handleSave}
              disabled={saving || loading}
              className="inline-flex items-center gap-1.5 px-4 py-2 rounded-xl text-xs font-semibold text-white transition-all disabled:opacity-40"
              style={{ background: 'var(--pc-accent)' }}
            >
              {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
              {t('common.save') || 'Save'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main Integrations Component
// ---------------------------------------------------------------------------

export default function Integrations() {
  const [integrations, setIntegrations] = useState<Integration[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeCategory, setActiveCategory] = useState<string>('all');
  const [configuring, setConfiguring] = useState<Integration | null>(null);

  const loadIntegrations = useCallback(() => {
    getIntegrations()
      .then(setIntegrations)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    loadIntegrations();
  }, [loadIntegrations]);

  const categories = ['all',
    ...Array.from(new Set(integrations.map((i) => i.category))).sort()
  ];
  const filtered =
    activeCategory === 'all'
      ? integrations
      : integrations.filter((i) => i.category === activeCategory);

  const grouped = filtered.reduce<Record<string, Integration[]>>((acc, item) => {
    const key = item.category;
    if (!acc[key]) acc[key] = [];
    acc[key].push(item);
    return acc;
  }, {});

  const getSchema = (integration: Integration): ChannelSchema | null => {
    if (integration.category.toLowerCase() !== 'chat') return null;
    if (integration.status === 'ComingSoon') return null;
    return CHANNEL_SCHEMAS[integration.name] ?? null;
  };

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'rgba(239, 68, 68, 0.08)', borderColor: 'rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
          {t('integrations.load_error')}: {error}
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
      <div className="flex items-center gap-2">
        <Puzzle className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          {t('integrations.title')} ({integrations.length})
        </h2>
      </div>

      {/* Category Filter Tabs */}
      <div className="flex flex-wrap gap-2">
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

      {/* Grouped Integration Cards */}
      {Object.keys(grouped).length === 0 ? (
        <div className="card p-8 text-center">
          <Puzzle className="h-10 w-10 mx-auto mb-3" style={{ color: 'var(--pc-text-faint)' }} />
          <p style={{ color: 'var(--pc-text-muted)' }}>{t('integrations.empty')}</p>
        </div>
      ) : (
        Object.entries(grouped).sort(([a], [b]) => a.localeCompare(b)).map(([category, items]) => (
          <div key={category}>
            <h3 className="text-[10px] font-semibold uppercase tracking-wider mb-3 capitalize" style={{ color: 'var(--pc-text-faint)' }}>
              {category}
            </h3>
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4 stagger-children">
              {items.map((integration) => {
                const badge = statusBadge(integration.status);
                const BadgeIcon = badge.icon;
                const schema = getSchema(integration);
                const isConfigurable = schema !== null;

                return (
                  <div
                    key={integration.name}
                    className={`card p-5 animate-slide-in-up ${isConfigurable ? 'cursor-pointer hover:border-[#0080ff40] hover:shadow-[0_0_20px_rgba(0,128,255,0.08)] group' : ''}`}
                    onClick={() => {
                      if (isConfigurable) setConfiguring(integration);
                    }}
                  >
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="flex items-center gap-2">
                          <h4 className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
                            {integration.name}
                          </h4>
                          {isConfigurable && (
                            <Settings2 className="h-3.5 w-3.5 group-hover:text-[#0080ff] transition-colors flex-shrink-0" style={{ color: 'var(--pc-text-faint)' }} />
                          )}
                        </div>
                        <p className="text-sm mt-1 line-clamp-2" style={{ color: 'var(--pc-text-muted)' }}>
                          {integration.description}
                        </p>
                        {isConfigurable && (
                          <p className="text-[10px] mt-2 group-hover:text-[#556080] transition-colors" style={{ color: 'var(--pc-text-faint)' }}>
                            {t('integrations.click_to_configure') || 'Click to configure'}
                          </p>
                        )}
                      </div>
                      <span
                        className="flex-shrink-0 inline-flex items-center gap-1 px-2.5 py-1 rounded-full text-[10px] font-semibold border"
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

      {/* Configure Modal */}
      {configuring && getSchema(configuring) && (
        <ConfigureModal
          integration={configuring}
          schema={getSchema(configuring)!}
          onClose={() => setConfiguring(null)}
          onSaved={() => {
            setConfiguring(null);
            loadIntegrations();
          }}
        />
      )}
    </div>
  );
}
