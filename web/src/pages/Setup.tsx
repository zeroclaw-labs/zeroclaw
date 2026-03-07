import { useState, useEffect } from 'react';
import {
  Wand2,
  Save,
  CheckCircle,
  AlertTriangle,
  Loader2,
} from 'lucide-react';
import { apiFetch } from '@/lib/api';

// ---------------------------------------------------------------------------
// Types (mirrors the Rust SetupConfig DTO — wizard steps 1–9)
// ---------------------------------------------------------------------------

interface ProviderInfo {
  name: string;
  display_name: string;
  local: boolean;
}

interface TelegramSetup {
  enabled: boolean;
  bot_token: string;
  allowed_users: string;
  stream_mode: string;
  draft_update_interval_ms: number;
  interrupt_on_new_message: boolean;
  mention_only: boolean;
}

interface DiscordSetup {
  enabled: boolean;
  bot_token: string;
  guild_id: string;
  allowed_users: string;
  listen_to_bots: boolean;
  mention_only: boolean;
}

interface SlackSetup {
  enabled: boolean;
  bot_token: string;
  app_token: string;
  channel_id: string;
  allowed_users: string;
}

interface MattermostSetup {
  enabled: boolean;
  url: string;
  bot_token: string;
  channel_id: string;
  allowed_users: string;
  thread_replies: boolean;
  mention_only: boolean;
}

interface WebhookSetup {
  enabled: boolean;
  port: number;
  secret: string;
}

interface IMessageSetup {
  enabled: boolean;
  allowed_contacts: string;
}

interface MatrixSetup {
  enabled: boolean;
  homeserver: string;
  access_token: string;
  user_id: string;
  device_id: string;
  room_id: string;
  allowed_users: string;
  mention_only: boolean;
}

interface SignalSetup {
  enabled: boolean;
  http_url: string;
  account: string;
  group_id: string;
  allowed_from: string;
  ignore_attachments: boolean;
  ignore_stories: boolean;
}

interface WhatsAppSetup {
  enabled: boolean;
  access_token: string;
  phone_number_id: string;
  verify_token: string;
  app_secret: string;
  session_path: string;
  pair_phone: string;
  pair_code: string;
  allowed_numbers: string;
}

interface LinqSetup {
  enabled: boolean;
  api_token: string;
  from_phone: string;
  signing_secret: string;
  allowed_senders: string;
}

interface WatiSetup {
  enabled: boolean;
  api_token: string;
  api_url: string;
  tenant_id: string;
  allowed_numbers: string;
}

interface NextcloudTalkSetup {
  enabled: boolean;
  base_url: string;
  app_token: string;
  webhook_secret: string;
  allowed_users: string;
}

interface IrcSetup {
  enabled: boolean;
  server: string;
  port: number;
  nickname: string;
  username: string;
  channels: string;
  allowed_users: string;
  server_password: string;
  nickserv_password: string;
  sasl_password: string;
  verify_tls: boolean;
}

interface LarkSetup {
  enabled: boolean;
  app_id: string;
  app_secret: string;
  encrypt_key: string;
  verification_token: string;
  allowed_users: string;
  mention_only: boolean;
  use_feishu: boolean;
  receive_mode: string;
  port: number;
}

interface FeishuSetup {
  enabled: boolean;
  app_id: string;
  app_secret: string;
  encrypt_key: string;
  verification_token: string;
  allowed_users: string;
  receive_mode: string;
  port: number;
}

interface DingTalkSetup {
  enabled: boolean;
  client_id: string;
  client_secret: string;
  allowed_users: string;
}

interface QQSetup {
  enabled: boolean;
  app_id: string;
  app_secret: string;
  allowed_users: string;
  receive_mode: string;
}

interface NostrSetup {
  enabled: boolean;
  private_key: string;
  relays: string;
  allowed_pubkeys: string;
}

interface EmailSetup {
  enabled: boolean;
  imap_host: string;
  imap_port: number;
  imap_folder: string;
  smtp_host: string;
  smtp_port: number;
  smtp_tls: boolean;
  username: string;
  password: string;
  from_address: string;
  idle_timeout_secs: number;
  allowed_senders: string;
}

interface ClawdTalkSetup {
  enabled: boolean;
  api_key: string;
  connection_id: string;
  from_number: string;
  allowed_destinations: string;
  webhook_secret: string;
}

interface ChannelsSetup {
  telegram: TelegramSetup;
  discord: DiscordSetup;
  slack: SlackSetup;
  mattermost: MattermostSetup;
  webhook: WebhookSetup;
  imessage: IMessageSetup;
  matrix: MatrixSetup;
  signal: SignalSetup;
  whatsapp: WhatsAppSetup;
  linq: LinqSetup;
  wati: WatiSetup;
  nextcloud_talk: NextcloudTalkSetup;
  irc: IrcSetup;
  lark: LarkSetup;
  feishu: FeishuSetup;
  dingtalk: DingTalkSetup;
  qq: QQSetup;
  nostr: NostrSetup;
  email: EmailSetup;
  clawdtalk: ClawdTalkSetup;
}

interface SetupConfig {
  workspace: { path: string };
  provider: { name: string; api_key: string; model: string; api_url: string };
  channels: ChannelsSetup;
  tunnel: {
    provider: string;
    cloudflare_token: string;
    ngrok_auth_token: string;
    ngrok_domain: string;
    tailscale_funnel: boolean;
    tailscale_hostname: string;
    custom_start_command: string;
  };
  tool_mode: {
    composio_enabled: boolean;
    composio_api_key: string;
    secrets_encrypt: boolean;
  };
  hardware: {
    enabled: boolean;
    transport: string;
    serial_port: string;
    baud_rate: number;
    probe_target: string;
    workspace_datasheets: boolean;
  };
  memory: {
    backend: string;
    auto_save: boolean;
    hygiene_enabled: boolean;
    archive_after_days: number;
    purge_after_days: number;
    embedding_cache_size: number;
  };
  project_context: {
    user_name: string;
    timezone: string;
    agent_name: string;
    communication_style: string;
  };
  autonomy: { level: string; max_actions_per_hour: number };
  gateway: { host: string; port: number };
}

// ---------------------------------------------------------------------------
// API helpers — uses apiFetch so the bearer token is sent automatically
// ---------------------------------------------------------------------------

async function fetchSetupConfig(): Promise<SetupConfig> {
  return apiFetch<SetupConfig>('/api/onboard/config');
}

async function saveSetupConfig(config: SetupConfig): Promise<void> {
  await apiFetch('/api/onboard/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}

async function fetchProviders(): Promise<ProviderInfo[]> {
  try {
    return await apiFetch<ProviderInfo[]>('/api/onboard/providers');
  } catch {
    return [];
  }
}

interface ModelInfo {
  id: string;
  label: string;
}

interface ModelsResponse {
  default: string;
  models: ModelInfo[];
}

async function fetchModels(provider: string): Promise<ModelsResponse> {
  try {
    return await apiFetch<ModelsResponse>(
      `/api/onboard/models?provider=${encodeURIComponent(provider)}`,
    );
  } catch {
    return { default: '', models: [] };
  }
}

async function triggerScaffold(): Promise<string> {
  const res = await apiFetch<{ ok: boolean; message: string }>(
    '/api/onboard/scaffold',
    { method: 'POST' },
  );
  return res.message;
}

// ---------------------------------------------------------------------------
// Shared style constants
// ---------------------------------------------------------------------------

const inputCls =
  'w-full px-3 py-2 bg-gray-800 border border-gray-700 rounded-lg text-sm text-white focus:outline-none focus:border-blue-500';
const inputSmCls =
  'w-32 px-3 py-2 bg-gray-800 border border-gray-700 rounded-lg text-sm text-white focus:outline-none focus:border-blue-500';

// Channel names for display
type ChannelKey = keyof ChannelsSetup;

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export default function Setup() {
  const [config, setConfig] = useState<SetupConfig | null>(null);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [defaultModel, setDefaultModel] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [scaffolding, setScaffolding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([fetchSetupConfig(), fetchProviders()])
      .then(([cfg, provs]) => {
        setConfig(cfg);
        setProviders(provs);
        // Fetch curated models for the initial provider
        fetchModels(cfg.provider.name).then((res) => {
          setModels(res.models);
          setDefaultModel(res.default);
        });
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    if (!success) return;
    const timer = setTimeout(() => setSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [success]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      await saveSetupConfig(config);
      setSuccess('Configuration saved successfully.');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save');
    } finally {
      setSaving(false);
    }
  };

  // Generic nested state updater
  const update = <K extends keyof SetupConfig>(
    section: K,
    field: string,
    value: unknown,
  ) => {
    if (!config) return;
    setConfig({
      ...config,
      [section]: { ...config[section], [field]: value },
    });
  };

  // Provider change — also refresh curated models
  const updateProvider = (field: string, value: string) => {
    update('provider', field, value);
    if (field === 'name') {
      fetchModels(value).then((res) => {
        setModels(res.models);
        setDefaultModel(res.default);
        // Reset model to new provider's default
        update('provider', 'model', res.default);
      });
    }
  };

  // Scaffold workspace files
  const handleScaffold = async () => {
    setScaffolding(true);
    setError(null);
    try {
      const msg = await triggerScaffold();
      setSuccess(msg);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Scaffold failed');
    } finally {
      setScaffolding(false);
    }
  };

  // Deep nested updater for channels.<channel>.<field>
  const updateChannel = (
    channel: ChannelKey,
    field: string,
    value: unknown,
  ) => {
    if (!config) return;
    setConfig({
      ...config,
      channels: {
        ...config.channels,
        [channel]: { ...config.channels[channel], [field]: value },
      },
    });
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  if (!config) {
    return (
      <div className="p-6">
        <div className="flex items-center gap-2 bg-red-900/30 border border-red-700 rounded-lg p-3">
          <AlertTriangle className="h-4 w-4 text-red-400 flex-shrink-0" />
          <span className="text-sm text-red-300">
            {error || 'Failed to load configuration.'}
          </span>
        </div>
      </div>
    );
  }

  const selectedProvider = providers.find((p) => p.name === config.provider.name);
  const isLocal = selectedProvider?.local ?? false;

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Wand2 className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">Setup Wizard</h2>
        </div>
        <button
          onClick={handleSave}
          disabled={saving}
          className="flex items-center gap-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium px-4 py-2 rounded-lg transition-colors disabled:opacity-50"
        >
          {saving ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Save className="h-4 w-4" />
          )}
          {saving ? 'Saving...' : 'Save Configuration'}
        </button>
      </div>

      {/* Status messages */}
      {success && (
        <div className="flex items-center gap-2 bg-green-900/30 border border-green-700 rounded-lg p-3">
          <CheckCircle className="h-4 w-4 text-green-400 flex-shrink-0" />
          <span className="text-sm text-green-300">{success}</span>
        </div>
      )}
      {error && (
        <div className="flex items-center gap-2 bg-red-900/30 border border-red-700 rounded-lg p-3">
          <AlertTriangle className="h-4 w-4 text-red-400 flex-shrink-0" />
          <span className="text-sm text-red-300">{error}</span>
        </div>
      )}

      {/* ── Step 1: Workspace ── */}
      <Card title="[1/9] Workspace">
        <Field label="Workspace path">
          <input
            type="text"
            value={config.workspace.path}
            onChange={(e) => update('workspace', 'path', e.target.value)}
            placeholder="~/.zeroclaw/workspace"
            className={inputCls}
          />
        </Field>
      </Card>

      {/* ── Step 2: AI Provider & API Key ── */}
      <Card title="[2/9] AI Provider & API Key">
        <Field label="Provider">
          <select
            value={config.provider.name}
            onChange={(e) => updateProvider('name', e.target.value)}
            className={inputCls}
          >
            {providers.map((p) => (
              <option key={p.name} value={p.name}>
                {p.display_name}
                {p.local ? ' (local)' : ''}
              </option>
            ))}
          </select>
        </Field>
        {!isLocal && (
          <Field label="API Key">
            <input
              type="password"
              value={config.provider.api_key}
              onChange={(e) => update('provider', 'api_key', e.target.value)}
              placeholder="sk-..."
              className={inputCls}
            />
          </Field>
        )}
        <Field label="Model">
          {models.length > 0 ? (
            <select
              value={config.provider.model || defaultModel}
              onChange={(e) => update('provider', 'model', e.target.value)}
              className={inputCls}
            >
              {models.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
              {/* Allow custom model entry if current value isn't in the list */}
              {config.provider.model &&
                !models.some((m) => m.id === config.provider.model) && (
                  <option value={config.provider.model}>
                    {config.provider.model} (custom)
                  </option>
                )}
            </select>
          ) : (
            <input
              type="text"
              value={config.provider.model}
              onChange={(e) => update('provider', 'model', e.target.value)}
              placeholder="e.g. gpt-4o, claude-sonnet-4-20250514"
              className={inputCls}
            />
          )}
          <p className="text-xs text-gray-500 mt-1">
            {defaultModel && `Default: ${defaultModel}`}
            {models.length > 0 && ' — or type a custom model name in config.toml'}
          </p>
        </Field>
        <Field label="API URL (optional)">
          <input
            type="text"
            value={config.provider.api_url}
            onChange={(e) => update('provider', 'api_url', e.target.value)}
            placeholder="https://..."
            className={inputCls}
          />
        </Field>
      </Card>

      {/* ── Step 3: Channels ── */}
      <Card title="[3/9] Channels">
        <p className="text-xs text-gray-500 mb-2">
          Channels let you talk to ZeroClaw from anywhere. CLI is always available.
          Use comma-separated values for user lists, or <code>*</code> for all.
        </p>

        {/* Telegram */}
        <ChannelSection title="Telegram">
          <Field label="Enable">
            <Toggle
              checked={config.channels.telegram.enabled}
              onChange={(v) => updateChannel('telegram', 'enabled', v)}
            />
          </Field>
          {config.channels.telegram.enabled && (
            <>
              <Field label="Bot Token">
                <input type="password" value={config.channels.telegram.bot_token}
                  onChange={(e) => updateChannel('telegram', 'bot_token', e.target.value)}
                  placeholder="123456:ABC-DEF..." className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.telegram.allowed_users}
                  onChange={(e) => updateChannel('telegram', 'allowed_users', e.target.value)}
                  placeholder="* (all) or user1, user2" className={inputCls} />
              </Field>
              <Field label="Stream Mode">
                <select value={config.channels.telegram.stream_mode}
                  onChange={(e) => updateChannel('telegram', 'stream_mode', e.target.value)}
                  className={inputSmCls}>
                  <option value="off">Off</option>
                  <option value="partial">Partial</option>
                </select>
              </Field>
              <Field label="Draft Update Interval (ms)">
                <input type="number" min={100} value={config.channels.telegram.draft_update_interval_ms}
                  onChange={(e) => updateChannel('telegram', 'draft_update_interval_ms', Number(e.target.value) || 500)}
                  className={inputSmCls} />
              </Field>
              <Field label="Interrupt on New Message">
                <Toggle checked={config.channels.telegram.interrupt_on_new_message}
                  onChange={(v) => updateChannel('telegram', 'interrupt_on_new_message', v)} />
              </Field>
              <Field label="Mention Only">
                <Toggle checked={config.channels.telegram.mention_only}
                  onChange={(v) => updateChannel('telegram', 'mention_only', v)} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Discord */}
        <ChannelSection title="Discord">
          <Field label="Enable">
            <Toggle checked={config.channels.discord.enabled}
              onChange={(v) => updateChannel('discord', 'enabled', v)} />
          </Field>
          {config.channels.discord.enabled && (
            <>
              <Field label="Bot Token">
                <input type="password" value={config.channels.discord.bot_token}
                  onChange={(e) => updateChannel('discord', 'bot_token', e.target.value)}
                  placeholder="Discord bot token" className={inputCls} />
              </Field>
              <Field label="Guild ID (optional)">
                <input type="text" value={config.channels.discord.guild_id}
                  onChange={(e) => updateChannel('discord', 'guild_id', e.target.value)}
                  placeholder="Server ID" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.discord.allowed_users}
                  onChange={(e) => updateChannel('discord', 'allowed_users', e.target.value)}
                  placeholder="* (all) or user1, user2" className={inputCls} />
              </Field>
              <Field label="Listen to Bots">
                <Toggle checked={config.channels.discord.listen_to_bots}
                  onChange={(v) => updateChannel('discord', 'listen_to_bots', v)} />
              </Field>
              <Field label="Mention Only">
                <Toggle checked={config.channels.discord.mention_only}
                  onChange={(v) => updateChannel('discord', 'mention_only', v)} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Slack */}
        <ChannelSection title="Slack">
          <Field label="Enable">
            <Toggle checked={config.channels.slack.enabled}
              onChange={(v) => updateChannel('slack', 'enabled', v)} />
          </Field>
          {config.channels.slack.enabled && (
            <>
              <Field label="Bot Token">
                <input type="password" value={config.channels.slack.bot_token}
                  onChange={(e) => updateChannel('slack', 'bot_token', e.target.value)}
                  placeholder="xoxb-..." className={inputCls} />
              </Field>
              <Field label="App Token (optional)">
                <input type="password" value={config.channels.slack.app_token}
                  onChange={(e) => updateChannel('slack', 'app_token', e.target.value)}
                  placeholder="xapp-..." className={inputCls} />
              </Field>
              <Field label="Channel ID (optional)">
                <input type="text" value={config.channels.slack.channel_id}
                  onChange={(e) => updateChannel('slack', 'channel_id', e.target.value)}
                  placeholder="C01234567" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.slack.allowed_users}
                  onChange={(e) => updateChannel('slack', 'allowed_users', e.target.value)}
                  placeholder="* (all) or user1, user2" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Mattermost */}
        <ChannelSection title="Mattermost">
          <Field label="Enable">
            <Toggle checked={config.channels.mattermost.enabled}
              onChange={(v) => updateChannel('mattermost', 'enabled', v)} />
          </Field>
          {config.channels.mattermost.enabled && (
            <>
              <Field label="Server URL">
                <input type="text" value={config.channels.mattermost.url}
                  onChange={(e) => updateChannel('mattermost', 'url', e.target.value)}
                  placeholder="https://mattermost.example.com" className={inputCls} />
              </Field>
              <Field label="Bot Token">
                <input type="password" value={config.channels.mattermost.bot_token}
                  onChange={(e) => updateChannel('mattermost', 'bot_token', e.target.value)}
                  placeholder="Bot token" className={inputCls} />
              </Field>
              <Field label="Channel ID (optional)">
                <input type="text" value={config.channels.mattermost.channel_id}
                  onChange={(e) => updateChannel('mattermost', 'channel_id', e.target.value)}
                  placeholder="Channel ID" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.mattermost.allowed_users}
                  onChange={(e) => updateChannel('mattermost', 'allowed_users', e.target.value)}
                  placeholder="* (all) or user1, user2" className={inputCls} />
              </Field>
              <Field label="Thread Replies">
                <Toggle checked={config.channels.mattermost.thread_replies}
                  onChange={(v) => updateChannel('mattermost', 'thread_replies', v)} />
              </Field>
              <Field label="Mention Only">
                <Toggle checked={config.channels.mattermost.mention_only}
                  onChange={(v) => updateChannel('mattermost', 'mention_only', v)} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* iMessage */}
        <ChannelSection title="iMessage (macOS only)">
          <Field label="Enable">
            <Toggle checked={config.channels.imessage.enabled}
              onChange={(v) => updateChannel('imessage', 'enabled', v)} />
          </Field>
          {config.channels.imessage.enabled && (
            <Field label="Allowed Contacts">
              <input type="text" value={config.channels.imessage.allowed_contacts}
                onChange={(e) => updateChannel('imessage', 'allowed_contacts', e.target.value)}
                placeholder="* (all) or contact1, contact2" className={inputCls} />
            </Field>
          )}
        </ChannelSection>

        {/* Matrix */}
        <ChannelSection title="Matrix">
          <Field label="Enable">
            <Toggle checked={config.channels.matrix.enabled}
              onChange={(v) => updateChannel('matrix', 'enabled', v)} />
          </Field>
          {config.channels.matrix.enabled && (
            <>
              <Field label="Homeserver URL">
                <input type="text" value={config.channels.matrix.homeserver}
                  onChange={(e) => updateChannel('matrix', 'homeserver', e.target.value)}
                  placeholder="https://matrix.org" className={inputCls} />
              </Field>
              <Field label="Access Token">
                <input type="password" value={config.channels.matrix.access_token}
                  onChange={(e) => updateChannel('matrix', 'access_token', e.target.value)}
                  placeholder="Access token" className={inputCls} />
              </Field>
              <Field label="User ID (optional)">
                <input type="text" value={config.channels.matrix.user_id}
                  onChange={(e) => updateChannel('matrix', 'user_id', e.target.value)}
                  placeholder="@bot:matrix.org" className={inputCls} />
              </Field>
              <Field label="Device ID (optional)">
                <input type="text" value={config.channels.matrix.device_id}
                  onChange={(e) => updateChannel('matrix', 'device_id', e.target.value)}
                  placeholder="Auto-detected" className={inputCls} />
              </Field>
              <Field label="Room ID">
                <input type="text" value={config.channels.matrix.room_id}
                  onChange={(e) => updateChannel('matrix', 'room_id', e.target.value)}
                  placeholder="!room:matrix.org" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.matrix.allowed_users}
                  onChange={(e) => updateChannel('matrix', 'allowed_users', e.target.value)}
                  placeholder="* (all) or @user1:matrix.org, @user2:matrix.org" className={inputCls} />
              </Field>
              <Field label="Mention Only">
                <label className="flex items-center gap-2">
                  <input type="checkbox" checked={config.channels.matrix.mention_only}
                    onChange={(e) => updateChannel('matrix', 'mention_only', e.target.checked)} />
                  <span className="text-sm text-gray-300">Only respond to @-mentions or direct rooms</span>
                </label>
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Signal */}
        <ChannelSection title="Signal">
          <Field label="Enable">
            <Toggle checked={config.channels.signal.enabled}
              onChange={(v) => updateChannel('signal', 'enabled', v)} />
          </Field>
          {config.channels.signal.enabled && (
            <>
              <Field label="HTTP URL">
                <input type="text" value={config.channels.signal.http_url}
                  onChange={(e) => updateChannel('signal', 'http_url', e.target.value)}
                  placeholder="http://127.0.0.1:8686" className={inputCls} />
              </Field>
              <Field label="Account (E.164)">
                <input type="text" value={config.channels.signal.account}
                  onChange={(e) => updateChannel('signal', 'account', e.target.value)}
                  placeholder="+1234567890" className={inputCls} />
              </Field>
              <Field label="Group ID (optional)">
                <input type="text" value={config.channels.signal.group_id}
                  onChange={(e) => updateChannel('signal', 'group_id', e.target.value)}
                  placeholder="Leave empty for DMs" className={inputCls} />
              </Field>
              <Field label="Allowed Senders">
                <input type="text" value={config.channels.signal.allowed_from}
                  onChange={(e) => updateChannel('signal', 'allowed_from', e.target.value)}
                  placeholder="* (all) or +1234567890, +0987654321" className={inputCls} />
              </Field>
              <Field label="Ignore Attachments">
                <Toggle checked={config.channels.signal.ignore_attachments}
                  onChange={(v) => updateChannel('signal', 'ignore_attachments', v)} />
              </Field>
              <Field label="Ignore Stories">
                <Toggle checked={config.channels.signal.ignore_stories}
                  onChange={(v) => updateChannel('signal', 'ignore_stories', v)} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* WhatsApp */}
        <ChannelSection title="WhatsApp">
          <Field label="Enable">
            <Toggle checked={config.channels.whatsapp.enabled}
              onChange={(v) => updateChannel('whatsapp', 'enabled', v)} />
          </Field>
          {config.channels.whatsapp.enabled && (
            <>
              <p className="text-xs text-gray-500 px-1">Business API mode:</p>
              <Field label="Access Token">
                <input type="password" value={config.channels.whatsapp.access_token}
                  onChange={(e) => updateChannel('whatsapp', 'access_token', e.target.value)}
                  placeholder="Business API token" className={inputCls} />
              </Field>
              <Field label="Phone Number ID">
                <input type="text" value={config.channels.whatsapp.phone_number_id}
                  onChange={(e) => updateChannel('whatsapp', 'phone_number_id', e.target.value)}
                  placeholder="Phone number ID" className={inputCls} />
              </Field>
              <Field label="Verify Token">
                <input type="password" value={config.channels.whatsapp.verify_token}
                  onChange={(e) => updateChannel('whatsapp', 'verify_token', e.target.value)}
                  placeholder="Webhook verify token" className={inputCls} />
              </Field>
              <Field label="App Secret">
                <input type="password" value={config.channels.whatsapp.app_secret}
                  onChange={(e) => updateChannel('whatsapp', 'app_secret', e.target.value)}
                  placeholder="App secret" className={inputCls} />
              </Field>
              <p className="text-xs text-gray-500 px-1 mt-2">Web/Pairing mode (alternative):</p>
              <Field label="Session Path">
                <input type="text" value={config.channels.whatsapp.session_path}
                  onChange={(e) => updateChannel('whatsapp', 'session_path', e.target.value)}
                  placeholder="Path to session storage" className={inputCls} />
              </Field>
              <Field label="Pair Phone">
                <input type="text" value={config.channels.whatsapp.pair_phone}
                  onChange={(e) => updateChannel('whatsapp', 'pair_phone', e.target.value)}
                  placeholder="Phone number to pair" className={inputCls} />
              </Field>
              <Field label="Pair Code">
                <input type="password" value={config.channels.whatsapp.pair_code}
                  onChange={(e) => updateChannel('whatsapp', 'pair_code', e.target.value)}
                  placeholder="Pairing code" className={inputCls} />
              </Field>
              <p className="text-xs text-gray-500 px-1 mt-2">Common:</p>
              <Field label="Allowed Numbers">
                <input type="text" value={config.channels.whatsapp.allowed_numbers}
                  onChange={(e) => updateChannel('whatsapp', 'allowed_numbers', e.target.value)}
                  placeholder="* (all) or +1234567890, +0987654321" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Linq */}
        <ChannelSection title="Linq">
          <Field label="Enable">
            <Toggle checked={config.channels.linq.enabled}
              onChange={(v) => updateChannel('linq', 'enabled', v)} />
          </Field>
          {config.channels.linq.enabled && (
            <>
              <Field label="API Token">
                <input type="password" value={config.channels.linq.api_token}
                  onChange={(e) => updateChannel('linq', 'api_token', e.target.value)}
                  placeholder="API token" className={inputCls} />
              </Field>
              <Field label="From Phone (E.164)">
                <input type="text" value={config.channels.linq.from_phone}
                  onChange={(e) => updateChannel('linq', 'from_phone', e.target.value)}
                  placeholder="+1234567890" className={inputCls} />
              </Field>
              <Field label="Signing Secret (optional)">
                <input type="password" value={config.channels.linq.signing_secret}
                  onChange={(e) => updateChannel('linq', 'signing_secret', e.target.value)}
                  placeholder="Signing secret" className={inputCls} />
              </Field>
              <Field label="Allowed Senders">
                <input type="text" value={config.channels.linq.allowed_senders}
                  onChange={(e) => updateChannel('linq', 'allowed_senders', e.target.value)}
                  placeholder="* (all) or sender1, sender2" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* WATI */}
        <ChannelSection title="WATI (WhatsApp Business)">
          <Field label="Enable">
            <Toggle checked={config.channels.wati.enabled}
              onChange={(v) => updateChannel('wati', 'enabled', v)} />
          </Field>
          {config.channels.wati.enabled && (
            <>
              <Field label="API Token">
                <input type="password" value={config.channels.wati.api_token}
                  onChange={(e) => updateChannel('wati', 'api_token', e.target.value)}
                  placeholder="API token" className={inputCls} />
              </Field>
              <Field label="API URL">
                <input type="text" value={config.channels.wati.api_url}
                  onChange={(e) => updateChannel('wati', 'api_url', e.target.value)}
                  placeholder="https://live-server-xxxxx.wati.io" className={inputCls} />
              </Field>
              <Field label="Tenant ID (optional)">
                <input type="text" value={config.channels.wati.tenant_id}
                  onChange={(e) => updateChannel('wati', 'tenant_id', e.target.value)}
                  placeholder="Tenant ID" className={inputCls} />
              </Field>
              <Field label="Allowed Numbers">
                <input type="text" value={config.channels.wati.allowed_numbers}
                  onChange={(e) => updateChannel('wati', 'allowed_numbers', e.target.value)}
                  placeholder="* (all) or +1234567890, +0987654321" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Nextcloud Talk */}
        <ChannelSection title="Nextcloud Talk">
          <Field label="Enable">
            <Toggle checked={config.channels.nextcloud_talk.enabled}
              onChange={(v) => updateChannel('nextcloud_talk', 'enabled', v)} />
          </Field>
          {config.channels.nextcloud_talk.enabled && (
            <>
              <Field label="Base URL">
                <input type="text" value={config.channels.nextcloud_talk.base_url}
                  onChange={(e) => updateChannel('nextcloud_talk', 'base_url', e.target.value)}
                  placeholder="https://cloud.example.com" className={inputCls} />
              </Field>
              <Field label="App Token">
                <input type="password" value={config.channels.nextcloud_talk.app_token}
                  onChange={(e) => updateChannel('nextcloud_talk', 'app_token', e.target.value)}
                  placeholder="App token" className={inputCls} />
              </Field>
              <Field label="Webhook Secret (optional)">
                <input type="password" value={config.channels.nextcloud_talk.webhook_secret}
                  onChange={(e) => updateChannel('nextcloud_talk', 'webhook_secret', e.target.value)}
                  placeholder="Webhook secret" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.nextcloud_talk.allowed_users}
                  onChange={(e) => updateChannel('nextcloud_talk', 'allowed_users', e.target.value)}
                  placeholder="* (all) or user1, user2" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* IRC */}
        <ChannelSection title="IRC">
          <Field label="Enable">
            <Toggle checked={config.channels.irc.enabled}
              onChange={(v) => updateChannel('irc', 'enabled', v)} />
          </Field>
          {config.channels.irc.enabled && (
            <>
              <Field label="Server">
                <input type="text" value={config.channels.irc.server}
                  onChange={(e) => updateChannel('irc', 'server', e.target.value)}
                  placeholder="irc.libera.chat" className={inputCls} />
              </Field>
              <Field label="Port">
                <input type="number" min={1} max={65535} value={config.channels.irc.port}
                  onChange={(e) => updateChannel('irc', 'port', Number(e.target.value) || 6697)}
                  className={inputSmCls} />
              </Field>
              <Field label="Nickname">
                <input type="text" value={config.channels.irc.nickname}
                  onChange={(e) => updateChannel('irc', 'nickname', e.target.value)}
                  placeholder="zeroclaw-bot" className={inputCls} />
              </Field>
              <Field label="Username (optional)">
                <input type="text" value={config.channels.irc.username}
                  onChange={(e) => updateChannel('irc', 'username', e.target.value)}
                  placeholder="Username" className={inputCls} />
              </Field>
              <Field label="Channels">
                <input type="text" value={config.channels.irc.channels}
                  onChange={(e) => updateChannel('irc', 'channels', e.target.value)}
                  placeholder="#channel1, #channel2" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.irc.allowed_users}
                  onChange={(e) => updateChannel('irc', 'allowed_users', e.target.value)}
                  placeholder="* (all) or nick1, nick2" className={inputCls} />
              </Field>
              <Field label="Server Password">
                <input type="password" value={config.channels.irc.server_password}
                  onChange={(e) => updateChannel('irc', 'server_password', e.target.value)}
                  placeholder="Optional" className={inputCls} />
              </Field>
              <Field label="NickServ Password">
                <input type="password" value={config.channels.irc.nickserv_password}
                  onChange={(e) => updateChannel('irc', 'nickserv_password', e.target.value)}
                  placeholder="Optional" className={inputCls} />
              </Field>
              <Field label="SASL Password">
                <input type="password" value={config.channels.irc.sasl_password}
                  onChange={(e) => updateChannel('irc', 'sasl_password', e.target.value)}
                  placeholder="Optional" className={inputCls} />
              </Field>
              <Field label="Verify TLS">
                <Toggle checked={config.channels.irc.verify_tls}
                  onChange={(v) => updateChannel('irc', 'verify_tls', v)} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Lark */}
        <ChannelSection title="Lark">
          <Field label="Enable">
            <Toggle checked={config.channels.lark.enabled}
              onChange={(v) => updateChannel('lark', 'enabled', v)} />
          </Field>
          {config.channels.lark.enabled && (
            <>
              <Field label="App ID">
                <input type="text" value={config.channels.lark.app_id}
                  onChange={(e) => updateChannel('lark', 'app_id', e.target.value)}
                  placeholder="cli_xxxxx" className={inputCls} />
              </Field>
              <Field label="App Secret">
                <input type="password" value={config.channels.lark.app_secret}
                  onChange={(e) => updateChannel('lark', 'app_secret', e.target.value)}
                  placeholder="App secret" className={inputCls} />
              </Field>
              <Field label="Encrypt Key (optional)">
                <input type="password" value={config.channels.lark.encrypt_key}
                  onChange={(e) => updateChannel('lark', 'encrypt_key', e.target.value)}
                  placeholder="Encrypt key" className={inputCls} />
              </Field>
              <Field label="Verification Token (optional)">
                <input type="password" value={config.channels.lark.verification_token}
                  onChange={(e) => updateChannel('lark', 'verification_token', e.target.value)}
                  placeholder="Verification token" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.lark.allowed_users}
                  onChange={(e) => updateChannel('lark', 'allowed_users', e.target.value)}
                  placeholder="* (all) or open_id1, open_id2" className={inputCls} />
              </Field>
              <Field label="Mention Only">
                <Toggle checked={config.channels.lark.mention_only}
                  onChange={(v) => updateChannel('lark', 'mention_only', v)} />
              </Field>
              <Field label="Use Feishu API">
                <Toggle checked={config.channels.lark.use_feishu}
                  onChange={(v) => updateChannel('lark', 'use_feishu', v)} />
              </Field>
              <Field label="Receive Mode">
                <select value={config.channels.lark.receive_mode}
                  onChange={(e) => updateChannel('lark', 'receive_mode', e.target.value)}
                  className={inputSmCls}>
                  <option value="websocket">WebSocket</option>
                  <option value="webhook">Webhook</option>
                </select>
              </Field>
              {config.channels.lark.receive_mode === 'webhook' && (
                <Field label="Webhook Port">
                  <input type="number" min={1} max={65535} value={config.channels.lark.port}
                    onChange={(e) => updateChannel('lark', 'port', Number(e.target.value) || 9000)}
                    className={inputSmCls} />
                </Field>
              )}
            </>
          )}
        </ChannelSection>

        {/* Feishu */}
        <ChannelSection title="Feishu">
          <Field label="Enable">
            <Toggle checked={config.channels.feishu.enabled}
              onChange={(v) => updateChannel('feishu', 'enabled', v)} />
          </Field>
          {config.channels.feishu.enabled && (
            <>
              <Field label="App ID">
                <input type="text" value={config.channels.feishu.app_id}
                  onChange={(e) => updateChannel('feishu', 'app_id', e.target.value)}
                  placeholder="cli_xxxxx" className={inputCls} />
              </Field>
              <Field label="App Secret">
                <input type="password" value={config.channels.feishu.app_secret}
                  onChange={(e) => updateChannel('feishu', 'app_secret', e.target.value)}
                  placeholder="App secret" className={inputCls} />
              </Field>
              <Field label="Encrypt Key (optional)">
                <input type="password" value={config.channels.feishu.encrypt_key}
                  onChange={(e) => updateChannel('feishu', 'encrypt_key', e.target.value)}
                  placeholder="Encrypt key" className={inputCls} />
              </Field>
              <Field label="Verification Token (optional)">
                <input type="password" value={config.channels.feishu.verification_token}
                  onChange={(e) => updateChannel('feishu', 'verification_token', e.target.value)}
                  placeholder="Verification token" className={inputCls} />
              </Field>
              <Field label="Allowed Users">
                <input type="text" value={config.channels.feishu.allowed_users}
                  onChange={(e) => updateChannel('feishu', 'allowed_users', e.target.value)}
                  placeholder="* (all) or open_id1, open_id2" className={inputCls} />
              </Field>
              <Field label="Receive Mode">
                <select value={config.channels.feishu.receive_mode}
                  onChange={(e) => updateChannel('feishu', 'receive_mode', e.target.value)}
                  className={inputSmCls}>
                  <option value="websocket">WebSocket</option>
                  <option value="webhook">Webhook</option>
                </select>
              </Field>
              {config.channels.feishu.receive_mode === 'webhook' && (
                <Field label="Webhook Port">
                  <input type="number" min={1} max={65535} value={config.channels.feishu.port}
                    onChange={(e) => updateChannel('feishu', 'port', Number(e.target.value) || 9000)}
                    className={inputSmCls} />
                </Field>
              )}
            </>
          )}
        </ChannelSection>

        {/* DingTalk */}
        <ChannelSection title="DingTalk">
          <Field label="Enable">
            <Toggle checked={config.channels.dingtalk.enabled}
              onChange={(v) => updateChannel('dingtalk', 'enabled', v)} />
          </Field>
          {config.channels.dingtalk.enabled && (
            <>
              <Field label="Client ID (AppKey)">
                <input type="text" value={config.channels.dingtalk.client_id}
                  onChange={(e) => updateChannel('dingtalk', 'client_id', e.target.value)}
                  placeholder="AppKey" className={inputCls} />
              </Field>
              <Field label="Client Secret (AppSecret)">
                <input type="password" value={config.channels.dingtalk.client_secret}
                  onChange={(e) => updateChannel('dingtalk', 'client_secret', e.target.value)}
                  placeholder="AppSecret" className={inputCls} />
              </Field>
              <Field label="Allowed Staff IDs">
                <input type="text" value={config.channels.dingtalk.allowed_users}
                  onChange={(e) => updateChannel('dingtalk', 'allowed_users', e.target.value)}
                  placeholder="staff_id1, staff_id2" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* QQ Official */}
        <ChannelSection title="QQ Official">
          <Field label="Enable">
            <Toggle checked={config.channels.qq.enabled}
              onChange={(v) => updateChannel('qq', 'enabled', v)} />
          </Field>
          {config.channels.qq.enabled && (
            <>
              <Field label="App ID">
                <input type="text" value={config.channels.qq.app_id}
                  onChange={(e) => updateChannel('qq', 'app_id', e.target.value)}
                  placeholder="App ID" className={inputCls} />
              </Field>
              <Field label="App Secret">
                <input type="password" value={config.channels.qq.app_secret}
                  onChange={(e) => updateChannel('qq', 'app_secret', e.target.value)}
                  placeholder="App secret" className={inputCls} />
              </Field>
              <Field label="Allowed User IDs">
                <input type="text" value={config.channels.qq.allowed_users}
                  onChange={(e) => updateChannel('qq', 'allowed_users', e.target.value)}
                  placeholder="user_id1, user_id2" className={inputCls} />
              </Field>
              <Field label="Receive Mode">
                <select value={config.channels.qq.receive_mode}
                  onChange={(e) => updateChannel('qq', 'receive_mode', e.target.value)}
                  className={inputCls}>
                  <option value="webhook">Webhook (default)</option>
                  <option value="websocket">WebSocket</option>
                </select>
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Nostr */}
        <ChannelSection title="Nostr">
          <Field label="Enable">
            <Toggle checked={config.channels.nostr.enabled}
              onChange={(v) => updateChannel('nostr', 'enabled', v)} />
          </Field>
          {config.channels.nostr.enabled && (
            <>
              <Field label="Private Key (hex or nsec)">
                <input type="password" value={config.channels.nostr.private_key}
                  onChange={(e) => updateChannel('nostr', 'private_key', e.target.value)}
                  placeholder="nsec1... or hex key" className={inputCls} />
              </Field>
              <Field label="Relay URLs">
                <input type="text" value={config.channels.nostr.relays}
                  onChange={(e) => updateChannel('nostr', 'relays', e.target.value)}
                  placeholder="wss://relay.damus.io, wss://nos.lol" className={inputCls} />
              </Field>
              <Field label="Allowed Pubkeys">
                <input type="text" value={config.channels.nostr.allowed_pubkeys}
                  onChange={(e) => updateChannel('nostr', 'allowed_pubkeys', e.target.value)}
                  placeholder="* (all) or npub1..., npub2..." className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Email */}
        <ChannelSection title="Email (IMAP/SMTP)">
          <Field label="Enable">
            <Toggle checked={config.channels.email.enabled}
              onChange={(v) => updateChannel('email', 'enabled', v)} />
          </Field>
          {config.channels.email.enabled && (
            <>
              <p className="text-xs text-gray-500 px-1">IMAP (receive):</p>
              <Field label="IMAP Host">
                <input type="text" value={config.channels.email.imap_host}
                  onChange={(e) => updateChannel('email', 'imap_host', e.target.value)}
                  placeholder="imap.gmail.com" className={inputCls} />
              </Field>
              <Field label="IMAP Port">
                <input type="number" min={1} max={65535} value={config.channels.email.imap_port}
                  onChange={(e) => updateChannel('email', 'imap_port', Number(e.target.value) || 993)}
                  className={inputSmCls} />
              </Field>
              <Field label="IMAP Folder">
                <input type="text" value={config.channels.email.imap_folder}
                  onChange={(e) => updateChannel('email', 'imap_folder', e.target.value)}
                  placeholder="INBOX" className={inputCls} />
              </Field>
              <p className="text-xs text-gray-500 px-1 mt-2">SMTP (send):</p>
              <Field label="SMTP Host">
                <input type="text" value={config.channels.email.smtp_host}
                  onChange={(e) => updateChannel('email', 'smtp_host', e.target.value)}
                  placeholder="smtp.gmail.com" className={inputCls} />
              </Field>
              <Field label="SMTP Port">
                <input type="number" min={1} max={65535} value={config.channels.email.smtp_port}
                  onChange={(e) => updateChannel('email', 'smtp_port', Number(e.target.value) || 587)}
                  className={inputSmCls} />
              </Field>
              <Field label="SMTP TLS">
                <Toggle checked={config.channels.email.smtp_tls}
                  onChange={(v) => updateChannel('email', 'smtp_tls', v)} />
              </Field>
              <p className="text-xs text-gray-500 px-1 mt-2">Credentials:</p>
              <Field label="Username">
                <input type="text" value={config.channels.email.username}
                  onChange={(e) => updateChannel('email', 'username', e.target.value)}
                  placeholder="user@example.com" className={inputCls} />
              </Field>
              <Field label="Password">
                <input type="password" value={config.channels.email.password}
                  onChange={(e) => updateChannel('email', 'password', e.target.value)}
                  placeholder="Password or app password" className={inputCls} />
              </Field>
              <Field label="From Address">
                <input type="text" value={config.channels.email.from_address}
                  onChange={(e) => updateChannel('email', 'from_address', e.target.value)}
                  placeholder="bot@example.com" className={inputCls} />
              </Field>
              <Field label="IDLE Timeout (secs)">
                <input type="number" min={30} value={config.channels.email.idle_timeout_secs}
                  onChange={(e) => updateChannel('email', 'idle_timeout_secs', Number(e.target.value) || 300)}
                  className={inputSmCls} />
              </Field>
              <Field label="Allowed Senders">
                <input type="text" value={config.channels.email.allowed_senders}
                  onChange={(e) => updateChannel('email', 'allowed_senders', e.target.value)}
                  placeholder="* (all) or user@example.com" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* Webhook */}
        <ChannelSection title="Webhook (Generic)">
          <Field label="Enable">
            <Toggle checked={config.channels.webhook.enabled}
              onChange={(v) => updateChannel('webhook', 'enabled', v)} />
          </Field>
          {config.channels.webhook.enabled && (
            <>
              <Field label="Port">
                <input type="number" min={1} max={65535} value={config.channels.webhook.port}
                  onChange={(e) => updateChannel('webhook', 'port', Number(e.target.value) || 8080)}
                  className={inputSmCls} />
              </Field>
              <Field label="Secret (optional)">
                <input type="password" value={config.channels.webhook.secret}
                  onChange={(e) => updateChannel('webhook', 'secret', e.target.value)}
                  placeholder="Webhook secret" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>

        {/* ClawdTalk */}
        <ChannelSection title="ClawdTalk">
          <Field label="Enable">
            <Toggle checked={config.channels.clawdtalk.enabled}
              onChange={(v) => updateChannel('clawdtalk', 'enabled', v)} />
          </Field>
          {config.channels.clawdtalk.enabled && (
            <>
              <Field label="API Key">
                <input type="password" value={config.channels.clawdtalk.api_key}
                  onChange={(e) => updateChannel('clawdtalk', 'api_key', e.target.value)}
                  placeholder="API key" className={inputCls} />
              </Field>
              <Field label="Connection ID">
                <input type="text" value={config.channels.clawdtalk.connection_id}
                  onChange={(e) => updateChannel('clawdtalk', 'connection_id', e.target.value)}
                  placeholder="Connection ID" className={inputCls} />
              </Field>
              <Field label="From Number">
                <input type="text" value={config.channels.clawdtalk.from_number}
                  onChange={(e) => updateChannel('clawdtalk', 'from_number', e.target.value)}
                  placeholder="+1234567890" className={inputCls} />
              </Field>
              <Field label="Allowed Destinations">
                <input type="text" value={config.channels.clawdtalk.allowed_destinations}
                  onChange={(e) => updateChannel('clawdtalk', 'allowed_destinations', e.target.value)}
                  placeholder="dest1, dest2" className={inputCls} />
              </Field>
              <Field label="Webhook Secret (optional)">
                <input type="password" value={config.channels.clawdtalk.webhook_secret}
                  onChange={(e) => updateChannel('clawdtalk', 'webhook_secret', e.target.value)}
                  placeholder="Webhook secret" className={inputCls} />
              </Field>
            </>
          )}
        </ChannelSection>
      </Card>

      {/* ── Step 4: Tunnel ── */}
      <Card title="[4/9] Tunnel (Expose to Internet)">
        <p className="text-xs text-gray-500 mb-2">
          A tunnel exposes your gateway to the internet securely. Skip if you only
          use CLI or local channels.
        </p>
        <Field label="Tunnel provider">
          <select
            value={config.tunnel.provider}
            onChange={(e) => update('tunnel', 'provider', e.target.value)}
            className={inputCls}
          >
            <option value="none">None (local only)</option>
            <option value="cloudflare">Cloudflare Tunnel</option>
            <option value="ngrok">ngrok</option>
            <option value="tailscale">Tailscale</option>
            <option value="custom">Custom</option>
          </select>
        </Field>
        {config.tunnel.provider === 'cloudflare' && (
          <Field label="Tunnel Token">
            <input type="password" value={config.tunnel.cloudflare_token}
              onChange={(e) => update('tunnel', 'cloudflare_token', e.target.value)}
              placeholder="Cloudflare tunnel token" className={inputCls} />
          </Field>
        )}
        {config.tunnel.provider === 'ngrok' && (
          <>
            <Field label="Auth Token">
              <input type="password" value={config.tunnel.ngrok_auth_token}
                onChange={(e) => update('tunnel', 'ngrok_auth_token', e.target.value)}
                placeholder="ngrok auth token" className={inputCls} />
            </Field>
            <Field label="Custom Domain (optional)">
              <input type="text" value={config.tunnel.ngrok_domain}
                onChange={(e) => update('tunnel', 'ngrok_domain', e.target.value)}
                placeholder="my-app.ngrok.io" className={inputCls} />
            </Field>
          </>
        )}
        {config.tunnel.provider === 'tailscale' && (
          <>
            <Field label="Enable Funnel (public access)">
              <Toggle checked={config.tunnel.tailscale_funnel}
                onChange={(v) => update('tunnel', 'tailscale_funnel', v)} />
            </Field>
            <Field label="Hostname (optional)">
              <input type="text" value={config.tunnel.tailscale_hostname}
                onChange={(e) => update('tunnel', 'tailscale_hostname', e.target.value)}
                placeholder="Custom tailnet hostname" className={inputCls} />
            </Field>
          </>
        )}
        {config.tunnel.provider === 'custom' && (
          <Field label="Start Command">
            <input type="text" value={config.tunnel.custom_start_command}
              onChange={(e) => update('tunnel', 'custom_start_command', e.target.value)}
              placeholder="bore local {port} --to bore.pub" className={inputCls} />
            <p className="text-xs text-gray-500 mt-1">
              Use <code>{'{port}'}</code> and <code>{'{host}'}</code> as placeholders.
            </p>
          </Field>
        )}
      </Card>

      {/* ── Step 5: Tool Mode & Security ── */}
      <Card title="[5/9] Tool Mode & Security">
        <Field label="Composio OAuth tools">
          <Toggle
            checked={config.tool_mode.composio_enabled}
            onChange={(v) => update('tool_mode', 'composio_enabled', v)}
          />
        </Field>
        {config.tool_mode.composio_enabled && (
          <Field label="Composio API Key">
            <input
              type="password"
              value={config.tool_mode.composio_api_key}
              onChange={(e) =>
                update('tool_mode', 'composio_api_key', e.target.value)
              }
              placeholder="composio api key"
              className={inputCls}
            />
          </Field>
        )}
        <Field label="Encrypt secrets in config">
          <Toggle
            checked={config.tool_mode.secrets_encrypt}
            onChange={(v) => update('tool_mode', 'secrets_encrypt', v)}
          />
        </Field>
        <p className="text-xs text-gray-500">
          {config.tool_mode.secrets_encrypt
            ? 'API keys will be encrypted at rest in config.toml.'
            : 'API keys will be stored as plaintext (not recommended).'}
        </p>
      </Card>

      {/* ── Step 6: Hardware ── */}
      <Card title="[6/9] Hardware (Physical World)">
        <p className="text-xs text-gray-500 mb-2">
          ZeroClaw can control physical hardware (LEDs, sensors, motors).
        </p>
        <Field label="Enable hardware">
          <Toggle
            checked={config.hardware.enabled}
            onChange={(v) => update('hardware', 'enabled', v)}
          />
        </Field>
        {config.hardware.enabled && (
          <>
            <Field label="Transport">
              <select
                value={config.hardware.transport}
                onChange={(e) =>
                  update('hardware', 'transport', e.target.value)
                }
                className={inputCls}
              >
                <option value="none">None</option>
                <option value="native">
                  Native (direct GPIO on Linux board)
                </option>
                <option value="serial">
                  Serial (Arduino/ESP32/Nucleo via USB)
                </option>
                <option value="probe">
                  Debug Probe (SWD/JTAG via probe-rs)
                </option>
              </select>
            </Field>
            {config.hardware.transport === 'serial' && (
              <>
                <Field label="Serial Port">
                  <input
                    type="text"
                    value={config.hardware.serial_port}
                    onChange={(e) =>
                      update('hardware', 'serial_port', e.target.value)
                    }
                    placeholder="/dev/ttyACM0"
                    className={inputCls}
                  />
                </Field>
                <Field label="Baud Rate">
                  <select
                    value={config.hardware.baud_rate}
                    onChange={(e) =>
                      update('hardware', 'baud_rate', Number(e.target.value))
                    }
                    className={inputSmCls}
                  >
                    <option value={9600}>9600</option>
                    <option value={57600}>57600</option>
                    <option value={115200}>115200</option>
                    <option value={230400}>230400</option>
                  </select>
                </Field>
              </>
            )}
            {config.hardware.transport === 'probe' && (
              <Field label="Target MCU">
                <input
                  type="text"
                  value={config.hardware.probe_target}
                  onChange={(e) =>
                    update('hardware', 'probe_target', e.target.value)
                  }
                  placeholder="STM32F411CEUx"
                  className={inputCls}
                />
              </Field>
            )}
            <Field label="Enable Datasheet RAG">
              <Toggle
                checked={config.hardware.workspace_datasheets}
                onChange={(v) => update('hardware', 'workspace_datasheets', v)}
              />
            </Field>
            <p className="text-xs text-gray-500">
              Index PDF schematics in workspace for AI pin lookups.
            </p>
          </>
        )}
      </Card>

      {/* ── Step 7: Memory ── */}
      <Card title="[7/9] Memory">
        <Field label="Backend">
          <select
            value={config.memory.backend}
            onChange={(e) => update('memory', 'backend', e.target.value)}
            className={inputCls}
          >
            <option value="sqlite">SQLite (fast, local, recommended)</option>
            <option value="lucid">Lucid (SQLite + embeddings, semantic search)</option>
            <option value="markdown">Markdown (human-readable, Git-friendly)</option>
            <option value="none">None (no memory, fresh each session)</option>
          </select>
        </Field>
        <Field label="Auto-save conversations">
          <Toggle
            checked={config.memory.auto_save}
            onChange={(v) => update('memory', 'auto_save', v)}
          />
        </Field>
        {(config.memory.backend === 'lucid' ||
          config.memory.backend === 'sqlite') && (
          <>
            <Field label="Enable memory hygiene">
              <Toggle
                checked={config.memory.hygiene_enabled}
                onChange={(v) => update('memory', 'hygiene_enabled', v)}
              />
            </Field>
            {config.memory.hygiene_enabled && (
              <>
                <Field label="Archive after (days)">
                  <input
                    type="number"
                    min={0}
                    value={config.memory.archive_after_days}
                    onChange={(e) =>
                      update(
                        'memory',
                        'archive_after_days',
                        Number(e.target.value) || 0,
                      )
                    }
                    className={inputSmCls}
                  />
                </Field>
                <Field label="Purge after (days)">
                  <input
                    type="number"
                    min={0}
                    value={config.memory.purge_after_days}
                    onChange={(e) =>
                      update(
                        'memory',
                        'purge_after_days',
                        Number(e.target.value) || 0,
                      )
                    }
                    className={inputSmCls}
                  />
                </Field>
                <Field label="Embedding cache size">
                  <input
                    type="number"
                    min={0}
                    value={config.memory.embedding_cache_size}
                    onChange={(e) =>
                      update(
                        'memory',
                        'embedding_cache_size',
                        Number(e.target.value) || 0,
                      )
                    }
                    className={inputSmCls}
                  />
                </Field>
              </>
            )}
          </>
        )}
      </Card>

      {/* ── Step 8: Project Context ── */}
      <Card title="[8/9] Project Context (Personalize Your Agent)">
        <Field label="Your Name">
          <input
            type="text"
            value={config.project_context.user_name}
            onChange={(e) => update('project_context', 'user_name', e.target.value)}
            placeholder="User"
            className={inputCls}
          />
        </Field>
        <Field label="Timezone">
          <select
            value={config.project_context.timezone}
            onChange={(e) => update('project_context', 'timezone', e.target.value)}
            className={inputCls}
          >
            <option value="US/Eastern">US/Eastern</option>
            <option value="US/Central">US/Central</option>
            <option value="US/Mountain">US/Mountain</option>
            <option value="US/Pacific">US/Pacific</option>
            <option value="Europe/London">Europe/London</option>
            <option value="Europe/Berlin">Europe/Berlin</option>
            <option value="Asia/Tokyo">Asia/Tokyo</option>
            <option value="Asia/Shanghai">Asia/Shanghai</option>
            <option value="UTC">UTC</option>
          </select>
        </Field>
        <Field label="Agent Name">
          <input
            type="text"
            value={config.project_context.agent_name}
            onChange={(e) => update('project_context', 'agent_name', e.target.value)}
            placeholder="ZeroClaw"
            className={inputCls}
          />
        </Field>
        <Field label="Communication Style">
          <select
            value={config.project_context.communication_style}
            onChange={(e) => update('project_context', 'communication_style', e.target.value)}
            className={inputCls}
          >
            <option value="Direct & concise">Direct & concise</option>
            <option value="Friendly & casual">Friendly & casual</option>
            <option value="Professional & polished">Professional & polished</option>
            <option value="Expressive & playful">Expressive & playful</option>
            <option value="Technical & detailed">Technical & detailed</option>
            <option value="Balanced">Balanced</option>
          </select>
        </Field>
        <p className="text-xs text-gray-500 mb-3">
          These settings personalize workspace files (IDENTITY.md, USER.md, etc.)
          that are created during onboarding.
        </p>
        <button
          onClick={handleScaffold}
          disabled={scaffolding}
          className="flex items-center gap-2 px-4 py-2 bg-indigo-600 hover:bg-indigo-700 disabled:opacity-50 rounded-lg text-sm font-medium text-white transition-colors"
        >
          {scaffolding ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <Wand2 className="w-4 h-4" />
          )}
          {scaffolding ? 'Creating files…' : 'Create Workspace Files'}
        </button>
      </Card>

      {/* ── Step 9: Autonomy & Gateway ── */}
      <Card title="[9/9] Autonomy & Gateway">
        <Field label="Autonomy Level">
          <select
            value={config.autonomy.level}
            onChange={(e) => update('autonomy', 'level', e.target.value)}
            className={inputCls}
          >
            <option value="read_only">Read Only — can observe but not act</option>
            <option value="supervised">
              Supervised — acts but requires approval (default)
            </option>
            <option value="full">Full — autonomous within policy</option>
          </select>
        </Field>
        <Field label="Max actions per hour">
          <input
            type="number"
            min={1}
            max={10000}
            value={config.autonomy.max_actions_per_hour}
            onChange={(e) =>
              update(
                'autonomy',
                'max_actions_per_hour',
                Number(e.target.value) || 1,
              )
            }
            className={inputSmCls}
          />
        </Field>
        <Field label="Gateway Host">
          <input
            type="text"
            value={config.gateway.host}
            onChange={(e) => update('gateway', 'host', e.target.value)}
            placeholder="127.0.0.1"
            className={inputCls}
          />
        </Field>
        <Field label="Gateway Port">
          <input
            type="number"
            min={1}
            max={65535}
            value={config.gateway.port}
            onChange={(e) =>
              update('gateway', 'port', Number(e.target.value) || 42617)
            }
            className={inputSmCls}
          />
        </Field>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Shared sub-components
// ---------------------------------------------------------------------------

function Card({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
      <div className="px-4 py-3 border-b border-gray-800 bg-gray-800/50">
        <h3 className="text-sm font-medium text-gray-300 uppercase tracking-wider">
          {title}
        </h3>
      </div>
      <div className="p-4 space-y-4">{children}</div>
    </div>
  );
}

function ChannelSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-3 border-b border-gray-800 pb-4 pt-2">
      <p className="text-xs font-medium text-gray-400 uppercase tracking-wider">
        {title}
      </p>
      {children}
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-4">
      <label className="text-sm text-gray-400 sm:w-48 flex-shrink-0">
        {label}
      </label>
      <div className="flex-1">{children}</div>
    </div>
  );
}

function Toggle({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
        checked ? 'bg-blue-600' : 'bg-gray-700'
      }`}
    >
      <span
        className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
          checked ? 'translate-x-6' : 'translate-x-1'
        }`}
      />
    </button>
  );
}
