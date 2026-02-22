import { useState, useEffect } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Loader2, AlertTriangle, Globe, Clock, Plug, Bell, Shield } from 'lucide-react';
import { getTenantConfig, updateTenantConfig } from '../api/tenants';
import { useToast } from '../hooks/useToast';

type ToolSettings = Record<string, Record<string, unknown>>;

interface ToolDef {
  key: string;
  label: string;
  description: string;
  fields?: ToolField[];
}

interface ToolField {
  key: string;
  label: string;
  type: 'text' | 'password' | 'select';
  help?: string;
  placeholder?: string;
  options?: { value: string; label: string }[];
}

interface ToolGroup {
  label: string;
  icon: React.ReactNode;
  tools: ToolDef[];
}

const TOOL_GROUPS: ToolGroup[] = [
  {
    label: 'Web Access',
    icon: <Globe className="h-4 w-4" />,
    tools: [
      {
        key: 'browser',
        label: 'Browser',
        description: 'Browse web pages and extract content',
      },
      {
        key: 'http_request',
        label: 'HTTP Requests',
        description: 'Make outbound HTTP/API calls',
        fields: [
          { key: 'allowed_domains', label: 'Allowed Domains', type: 'text', placeholder: 'example.com, api.example.com', help: 'Comma-separated list of allowed domains (empty = all)' },
        ],
      },
      {
        key: 'web_search',
        label: 'Web Search',
        description: 'Search the web for information',
        fields: [
          { key: 'provider', label: 'Provider', type: 'select', options: [
            { value: 'tavily', label: 'Tavily' },
            { value: 'serper', label: 'Serper' },
            { value: 'brave', label: 'Brave Search' },
          ]},
          { key: 'api_key', label: 'API Key', type: 'password', placeholder: 'tvly-...' },
        ],
      },
    ],
  },
  {
    label: 'Scheduling',
    icon: <Clock className="h-4 w-4" />,
    tools: [
      {
        key: 'cron',
        label: 'Cron Jobs',
        description: 'Schedule recurring tasks with cron expressions',
      },
      {
        key: 'scheduler',
        label: 'Scheduler',
        description: 'Schedule one-time or delayed tasks',
      },
    ],
  },
  {
    label: 'Integrations',
    icon: <Plug className="h-4 w-4" />,
    tools: [
      {
        key: 'composio',
        label: 'Composio',
        description: 'Connect 150+ apps via Composio',
        fields: [
          { key: 'api_key', label: 'API Key', type: 'password', placeholder: 'composio-...' },
        ],
      },
    ],
  },
  {
    label: 'Notifications',
    icon: <Bell className="h-4 w-4" />,
    tools: [
      {
        key: 'pushover',
        label: 'Pushover',
        description: 'Send push notifications via Pushover',
        fields: [
          { key: 'user_key', label: 'User Key', type: 'password' },
          { key: 'app_token', label: 'App Token', type: 'password' },
        ],
      },
    ],
  },
  {
    label: 'Autonomy',
    icon: <Shield className="h-4 w-4" />,
    tools: [
      {
        key: 'autonomy',
        label: 'Agent Autonomy',
        description: 'Control how independently the agent can act',
        fields: [
          { key: 'level', label: 'Level', type: 'select', options: [
            { value: 'supervised', label: 'Supervised' },
            { value: 'semi-autonomous', label: 'Semi-Autonomous' },
            { value: 'autonomous', label: 'Autonomous' },
          ]},
          { key: 'allowed_commands', label: 'Allowed Shell Commands', type: 'text', placeholder: 'ls, cat, grep, curl', help: 'Comma-separated list of allowed shell commands' },
        ],
      },
    ],
  },
];

interface Props {
  tenantId: string;
}

export default function ToolsTab({ tenantId }: Props) {
  const qc = useQueryClient();
  const toast = useToast();
  const [settings, setSettings] = useState<ToolSettings>({});
  const [dirty, setDirty] = useState(false);

  const { data: config, isLoading } = useQuery({
    queryKey: ['config', tenantId],
    queryFn: () => getTenantConfig(tenantId),
  });

  // Seed local state from server
  useEffect(() => {
    if (config?.tool_settings) {
      setSettings(config.tool_settings);
      setDirty(false);
    }
  }, [config]);

  const saveMut = useMutation({
    mutationFn: (toolSettings: ToolSettings) =>
      updateTenantConfig(tenantId, { tool_settings: toolSettings }),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['config', tenantId] });
      setDirty(false);
      toast.success('Tools updated. Agent will restart.');
    },
    onError: (err: Error) => toast.error(err.message || 'Failed to save tools'),
  });

  function isEnabled(toolKey: string): boolean {
    return settings[toolKey]?.enabled === true;
  }

  function toggleTool(toolKey: string) {
    const current = settings[toolKey] ?? {};
    setSettings(prev => ({
      ...prev,
      [toolKey]: { ...current, enabled: !current.enabled },
    }));
    setDirty(true);
  }

  function setToolField(toolKey: string, fieldKey: string, value: string) {
    const current = settings[toolKey] ?? {};
    setSettings(prev => ({
      ...prev,
      [toolKey]: { ...current, [fieldKey]: value },
    }));
    setDirty(true);
  }

  function getToolField(toolKey: string, fieldKey: string): string {
    const val = settings[toolKey]?.[fieldKey];
    return val != null ? String(val) : '';
  }

  if (isLoading) {
    return (
      <div className="flex items-center gap-2 text-text-muted">
        <Loader2 className="h-5 w-5 animate-spin" />
        <span>Loading tool settings...</span>
      </div>
    );
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-lg font-semibold text-text-primary">Tools & Skills</h2>
          <p className="text-xs text-text-muted">
            Enable or disable agent capabilities. Changes require a container restart.
          </p>
        </div>
      </div>

      <div className="space-y-6">
        {TOOL_GROUPS.map(group => (
          <div key={group.label}>
            <div className="flex items-center gap-2 mb-3">
              <span className="text-text-muted">{group.icon}</span>
              <h3 className="text-sm font-semibold text-text-secondary uppercase tracking-wider">{group.label}</h3>
            </div>
            <div className="card p-0 overflow-hidden divide-y divide-border-subtle">
              {group.tools.map(tool => (
                <div key={tool.key} className="p-4">
                  <div className="flex items-center justify-between">
                    <div className="min-w-0">
                      <p className="text-sm font-medium text-text-primary">{tool.label}</p>
                      <p className="text-xs text-text-muted">{tool.description}</p>
                    </div>
                    <button
                      onClick={() => toggleTool(tool.key)}
                      className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors flex-shrink-0 ml-4 ${
                        isEnabled(tool.key) ? 'bg-accent-blue' : 'bg-gray-700'
                      }`}
                    >
                      <span
                        className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                          isEnabled(tool.key) ? 'translate-x-6' : 'translate-x-1'
                        }`}
                      />
                    </button>
                  </div>

                  {/* Expanded config fields when enabled */}
                  {isEnabled(tool.key) && tool.fields && tool.fields.length > 0 && (
                    <div className="mt-3 pt-3 border-t border-border-subtle space-y-3">
                      {tool.fields.map(field => (
                        <div key={field.key}>
                          <label className="block text-xs font-medium text-text-secondary mb-1">
                            {field.label}
                          </label>
                          {field.type === 'select' && field.options ? (
                            <select
                              value={getToolField(tool.key, field.key)}
                              onChange={e => setToolField(tool.key, field.key, e.target.value)}
                              className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
                            >
                              <option value="">Select...</option>
                              {field.options.map(opt => (
                                <option key={opt.value} value={opt.value}>{opt.label}</option>
                              ))}
                            </select>
                          ) : (
                            <input
                              type={field.type === 'password' ? 'password' : 'text'}
                              value={getToolField(tool.key, field.key)}
                              onChange={e => setToolField(tool.key, field.key, e.target.value)}
                              placeholder={field.placeholder}
                              className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
                            />
                          )}
                          {field.help && (
                            <p className="text-xs text-text-muted mt-1">{field.help}</p>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>

      {/* Save bar */}
      {dirty && (
        <div className="sticky bottom-0 mt-6 bg-bg-card border border-border-default rounded-xl p-4 flex items-center justify-between shadow-lg">
          <div className="flex items-center gap-2 text-xs text-amber-400">
            <AlertTriangle className="h-4 w-4" />
            <span>Unsaved changes. Saving will restart the agent container.</span>
          </div>
          <div className="flex gap-2">
            <button
              onClick={() => { if (config?.tool_settings) setSettings(config.tool_settings); setDirty(false); }}
              className="px-3 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors"
            >
              Discard
            </button>
            <button
              onClick={() => saveMut.mutate(settings)}
              disabled={saveMut.isPending}
              className="px-4 py-2 text-sm bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center gap-2"
            >
              {saveMut.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              {saveMut.isPending ? 'Saving...' : 'Save & Restart'}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
