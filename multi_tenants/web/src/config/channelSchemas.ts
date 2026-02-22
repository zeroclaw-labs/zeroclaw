export interface FieldDef {
  key: string;
  label: string;
  type: 'text' | 'password' | 'url' | 'number';
  required: boolean;
  help?: string;
  placeholder?: string;
}

export const CHANNEL_SCHEMAS: Record<string, FieldDef[]> = {
  telegram: [
    {
      key: 'bot_token',
      label: 'Bot Token',
      type: 'password',
      required: true,
      help: 'Get from @BotFather on Telegram',
      placeholder: '123456:ABC-DEF...',
    },
    {
      key: 'chat_id',
      label: 'Allowed Users',
      type: 'text',
      required: false,
      help: 'Enter * for open access (anyone can chat), or a specific user/chat ID. Leave empty for manual approval mode.',
      placeholder: '* or 123456789',
    },
  ],
  discord: [
    {
      key: 'bot_token',
      label: 'Bot Token',
      type: 'password',
      required: true,
      help: 'Discord Developer Portal > Bot > Token',
    },
    {
      key: 'guild_id',
      label: 'Server ID',
      type: 'text',
      required: true,
      help: 'Right-click server > Copy Server ID',
    },
  ],
  slack: [
    {
      key: 'bot_token',
      label: 'Bot Token (xoxb-...)',
      type: 'password',
      required: true,
      help: 'From Slack App > OAuth & Permissions',
    },
    {
      key: 'app_token',
      label: 'App Token (xapp-...)',
      type: 'password',
      required: false,
      help: 'From Slack App > Basic Information > App-Level Tokens (for Socket Mode)',
    },
    {
      key: 'channel_id',
      label: 'Channel ID',
      type: 'text',
      required: false,
      help: 'Right-click channel > View channel details > Copy Channel ID',
    },
  ],
  webhook: [
    {
      key: 'url',
      label: 'Webhook URL',
      type: 'url',
      required: true,
      placeholder: 'https://example.com/webhook',
    },
    {
      key: 'secret',
      label: 'Signing Secret',
      type: 'password',
      required: false,
      help: 'HMAC secret for payload verification (optional)',
    },
  ],
  mattermost: [
    {
      key: 'url',
      label: 'Server URL',
      type: 'url',
      required: true,
      placeholder: 'https://mattermost.example.com',
      help: 'Your Mattermost server URL',
    },
    {
      key: 'bot_token',
      label: 'Bot Token',
      type: 'password',
      required: true,
      help: 'From Mattermost > Integrations > Bot Accounts',
    },
    {
      key: 'channel_id',
      label: 'Channel ID',
      type: 'text',
      required: false,
      help: 'Channel to post in (optional)',
    },
    {
      key: 'team_id',
      label: 'Team ID',
      type: 'text',
      required: false,
    },
  ],
  whatsapp: [
    {
      key: 'phone_number_id',
      label: 'Phone Number ID',
      type: 'text',
      required: true,
      help: 'From Meta Developer Portal > WhatsApp > Phone Numbers',
    },
    {
      key: 'access_token',
      label: 'Access Token',
      type: 'password',
      required: true,
      help: 'Permanent token from Meta Developer Portal',
    },
    {
      key: 'verify_token',
      label: 'Webhook Verify Token',
      type: 'text',
      required: false,
      help: 'Custom string for webhook verification',
    },
    {
      key: 'app_secret',
      label: 'App Secret',
      type: 'password',
      required: false,
      help: 'From Meta Developer Portal > App Settings',
    },
  ],
  email: [
    {
      key: 'imap_host',
      label: 'IMAP Host',
      type: 'text',
      required: true,
      placeholder: 'imap.gmail.com',
    },
    {
      key: 'imap_port',
      label: 'IMAP Port',
      type: 'number',
      required: false,
      placeholder: '993',
      help: 'Default: 993 (SSL)',
    },
    {
      key: 'smtp_host',
      label: 'SMTP Host',
      type: 'text',
      required: true,
      placeholder: 'smtp.gmail.com',
    },
    {
      key: 'smtp_port',
      label: 'SMTP Port',
      type: 'number',
      required: false,
      placeholder: '465',
      help: 'Default: 465 (SSL)',
    },
    {
      key: 'username',
      label: 'Username',
      type: 'text',
      required: true,
      placeholder: 'bot@example.com',
    },
    {
      key: 'password',
      label: 'Password',
      type: 'password',
      required: true,
      help: 'Email password or app-specific password',
    },
    {
      key: 'from_address',
      label: 'From Address',
      type: 'text',
      required: true,
      placeholder: 'bot@example.com',
    },
  ],
  irc: [
    {
      key: 'server',
      label: 'Server',
      type: 'text',
      required: true,
      placeholder: 'irc.libera.chat',
    },
    {
      key: 'port',
      label: 'Port',
      type: 'number',
      required: false,
      placeholder: '6697',
      help: 'Default: 6697 (TLS)',
    },
    {
      key: 'nickname',
      label: 'Nickname',
      type: 'text',
      required: true,
      placeholder: 'zeroclaw-bot',
    },
    {
      key: 'channels',
      label: 'Channels',
      type: 'text',
      required: false,
      placeholder: '#general, #random',
      help: 'Comma-separated list of channels to join',
    },
    {
      key: 'server_password',
      label: 'Server Password',
      type: 'password',
      required: false,
    },
  ],
  matrix: [
    {
      key: 'homeserver',
      label: 'Homeserver URL',
      type: 'url',
      required: true,
      placeholder: 'https://matrix.org',
    },
    {
      key: 'access_token',
      label: 'Access Token',
      type: 'password',
      required: true,
      help: 'From Element > Settings > Help & About > Access Token',
    },
    {
      key: 'room_id',
      label: 'Room ID',
      type: 'text',
      required: true,
      placeholder: '!abc123:matrix.org',
      help: 'From Room Settings > Advanced > Internal Room ID',
    },
  ],
  signal: [
    {
      key: 'http_url',
      label: 'Signal CLI REST API URL',
      type: 'url',
      required: true,
      placeholder: 'http://localhost:8080',
      help: 'URL of signal-cli-rest-api instance',
    },
    {
      key: 'account',
      label: 'Phone Number',
      type: 'text',
      required: true,
      placeholder: '+1234567890',
      help: 'Registered Signal phone number (E.164 format)',
    },
    {
      key: 'group_id',
      label: 'Group ID',
      type: 'text',
      required: false,
      help: 'Signal group ID, or leave empty for direct messages only',
    },
  ],
  lark: [
    {
      key: 'app_id',
      label: 'App ID',
      type: 'text',
      required: true,
      help: 'From Lark/Feishu Open Platform > App Credentials',
    },
    {
      key: 'app_secret',
      label: 'App Secret',
      type: 'password',
      required: true,
    },
    {
      key: 'verification_token',
      label: 'Verification Token',
      type: 'text',
      required: false,
      help: 'For event subscription verification',
    },
  ],
  dingtalk: [
    {
      key: 'client_id',
      label: 'App Key (Client ID)',
      type: 'text',
      required: true,
      help: 'From DingTalk Open Platform > App Details',
    },
    {
      key: 'client_secret',
      label: 'App Secret',
      type: 'password',
      required: true,
    },
  ],
  qq: [
    {
      key: 'app_id',
      label: 'App ID',
      type: 'text',
      required: true,
      help: 'From QQ Bot Open Platform',
    },
    {
      key: 'app_secret',
      label: 'App Secret',
      type: 'password',
      required: true,
    },
  ],
};

export const PLAN_LIMITS: Record<string, { messages: number; channels: number; members: number; memory: string }> = {
  free:       { messages: 100,  channels: 2,  members: 3,   memory: '128MB' },
  starter:    { messages: 500,  channels: 5,  members: 10,  memory: '192MB' },
  pro:        { messages: 1000, channels: 10, members: 20,  memory: '256MB' },
  enterprise: { messages: -1,   channels: 50, members: 100, memory: '512MB' },
};
