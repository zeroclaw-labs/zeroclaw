export interface ToolFieldDef {
  key: string;
  label: string;
  type: 'toggle' | 'text' | 'password' | 'select' | 'array' | 'number';
  required?: boolean;
  help?: string;
  placeholder?: string;
  options?: { value: string; label: string }[];
  defaultValue?: unknown;
}

export interface ToolDef {
  id: string;
  label: string;
  description: string;
  icon: string;
  fields: ToolFieldDef[];
}

export interface ToolCategory {
  id: string;
  label: string;
  description: string;
  color: string;      // tailwind color key: blue, violet, amber, emerald, rose
  iconBg: string;     // tailwind bg class for tool icon circle
  accentBorder: string; // tailwind border-l class
  headerBg: string;   // tailwind bg class for category header
  badgeBg: string;    // tailwind bg class for enabled count badge
  badgeText: string;   // tailwind text class for badge
  tools: ToolDef[];
}

export const TOOL_CATEGORIES: ToolCategory[] = [
  {
    id: 'web',
    label: 'Web Access',
    description: 'Browser, HTTP requests, and web search capabilities',
    color: 'blue',
    iconBg: 'bg-blue-50',
    accentBorder: 'border-l-blue-500',
    headerBg: 'bg-gradient-to-r from-blue-50 to-white',
    badgeBg: 'bg-blue-100',
    badgeText: 'text-blue-700',
    tools: [
      {
        id: 'browser',
        label: 'Browser',
        description: 'Headless browser for web page interaction',
        icon: '🌐',
        fields: [
          { key: 'enabled', label: 'Enable Browser', type: 'toggle', defaultValue: false },
        ],
      },
      {
        id: 'http_request',
        label: 'HTTP Requests',
        description: 'Make outbound HTTP requests to APIs',
        icon: '🔗',
        fields: [
          { key: 'enabled', label: 'Enable HTTP Requests', type: 'toggle', defaultValue: false },
          { key: 'allowed_domains', label: 'Allowed Domains', type: 'array',
            help: 'Comma-separated list of allowed domains. Empty = all domains.',
            placeholder: 'api.example.com, data.example.org' },
        ],
      },
      {
        id: 'web_search',
        label: 'Web Search',
        description: 'Search the web for information',
        icon: '🔍',
        fields: [
          { key: 'enabled', label: 'Enable Web Search', type: 'toggle', defaultValue: false },
          { key: 'provider', label: 'Provider', type: 'select',
            options: [
              { value: 'google', label: 'Google' },
              { value: 'bing', label: 'Bing' },
              { value: 'duckduckgo', label: 'DuckDuckGo' },
            ] },
          { key: 'api_key', label: 'API Key', type: 'password',
            help: 'Search provider API key', placeholder: 'Enter API key' },
        ],
      },
    ],
  },
  {
    id: 'scheduling',
    label: 'Scheduling',
    description: 'Cron jobs and scheduled task execution',
    color: 'violet',
    iconBg: 'bg-violet-50',
    accentBorder: 'border-l-violet-500',
    headerBg: 'bg-gradient-to-r from-violet-50 to-white',
    badgeBg: 'bg-violet-100',
    badgeText: 'text-violet-700',
    tools: [
      {
        id: 'cron',
        label: 'Cron Jobs',
        description: 'Schedule recurring tasks',
        icon: '⏰',
        fields: [
          { key: 'enabled', label: 'Enable Cron', type: 'toggle', defaultValue: false },
        ],
      },
      {
        id: 'scheduler',
        label: 'Scheduler',
        description: 'One-time scheduled task execution',
        icon: '📅',
        fields: [
          { key: 'enabled', label: 'Enable Scheduler', type: 'toggle', defaultValue: false },
        ],
      },
    ],
  },
  {
    id: 'notifications',
    label: 'Notifications',
    description: 'Push notifications and alerts',
    color: 'amber',
    iconBg: 'bg-amber-50',
    accentBorder: 'border-l-amber-500',
    headerBg: 'bg-gradient-to-r from-amber-50 to-white',
    badgeBg: 'bg-amber-100',
    badgeText: 'text-amber-700',
    tools: [
      {
        id: 'pushover',
        label: 'Pushover',
        description: 'Send push notifications via Pushover',
        icon: '🔔',
        fields: [
          { key: 'enabled', label: 'Enable Pushover', type: 'toggle', defaultValue: false },
          { key: 'user_key', label: 'User Key', type: 'password', placeholder: 'Pushover user key' },
          { key: 'app_token', label: 'App Token', type: 'password', placeholder: 'Pushover app token' },
        ],
      },
    ],
  },
  {
    id: 'integrations',
    label: 'Integrations',
    description: 'Third-party service integrations',
    color: 'emerald',
    iconBg: 'bg-emerald-50',
    accentBorder: 'border-l-emerald-500',
    headerBg: 'bg-gradient-to-r from-emerald-50 to-white',
    badgeBg: 'bg-emerald-100',
    badgeText: 'text-emerald-700',
    tools: [
      {
        id: 'composio',
        label: 'Composio',
        description: 'Connect to 200+ third-party services',
        icon: '🧩',
        fields: [
          { key: 'enabled', label: 'Enable Composio', type: 'toggle', defaultValue: false },
          { key: 'api_key', label: 'API Key', type: 'password',
            help: 'Get from composio.dev', placeholder: 'Composio API key' },
        ],
      },
    ],
  },
  {
    id: 'autonomy',
    label: 'Autonomy & Safety',
    description: 'Control agent autonomy level and command restrictions',
    color: 'rose',
    iconBg: 'bg-rose-50',
    accentBorder: 'border-l-rose-500',
    headerBg: 'bg-gradient-to-r from-rose-50 to-white',
    badgeBg: 'bg-rose-100',
    badgeText: 'text-rose-700',
    tools: [
      {
        id: 'autonomy',
        label: 'Autonomy Settings',
        description: 'Control what the agent can do independently',
        icon: '🛡️',
        fields: [
          { key: 'level', label: 'Autonomy Level', type: 'select',
            options: [
              { value: 'readonly', label: 'Read Only — Agent can only read, no writes' },
              { value: 'supervised', label: 'Supervised — Agent asks before acting' },
              { value: 'full', label: 'Full Autonomy — Agent acts independently' },
            ],
            defaultValue: 'supervised' },
          { key: 'workspace_only', label: 'Workspace Only', type: 'toggle', defaultValue: true,
            help: 'Restrict file operations to the workspace directory' },
          { key: 'allowed_commands', label: 'Allowed Commands', type: 'array',
            help: 'Comma-separated shell commands the agent can use',
            placeholder: 'ls, cat, grep, find' },
          { key: 'forbidden_paths', label: 'Forbidden Paths', type: 'array',
            help: 'Comma-separated paths the agent cannot access',
            placeholder: '/etc, /root, /var/log' },
        ],
      },
    ],
  },
];
