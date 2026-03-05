export type ChatChannelSupportLevel = 'Built-in' | 'Plugin' | 'Legacy';

export interface ChatChannelSupport {
  id: string;
  name: string;
  supportLevel: ChatChannelSupportLevel;
  summary: string;
  details?: string;
  recommended?: boolean;
}

export const CHAT_CHANNEL_SUPPORT: ChatChannelSupport[] = [
  {
    id: 'bluebubbles',
    name: 'BlueBubbles',
    supportLevel: 'Built-in',
    recommended: true,
    summary: 'Recommended for iMessage with BlueBubbles macOS server REST API.',
    details:
      'Supports edit, unsend, effects, reactions, and group management. Edit is currently broken on macOS 26 Tahoe.',
  },
  {
    id: 'discord',
    name: 'Discord',
    supportLevel: 'Built-in',
    summary: 'Discord Bot API + Gateway for servers, channels, and direct messages.',
  },
  {
    id: 'feishu',
    name: 'Feishu',
    supportLevel: 'Plugin',
    summary: 'Feishu/Lark bot integration over WebSocket.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'google-chat',
    name: 'Google Chat',
    supportLevel: 'Built-in',
    summary: 'Google Chat app integration via HTTP webhook.',
  },
  {
    id: 'imessage-legacy',
    name: 'iMessage (legacy)',
    supportLevel: 'Legacy',
    summary: 'Legacy macOS integration via imsg CLI.',
    details: 'Deprecated path for new setups; BlueBubbles is recommended.',
  },
  {
    id: 'irc',
    name: 'IRC',
    supportLevel: 'Built-in',
    summary: 'Classic IRC channels and DMs with pairing and allowlist controls.',
  },
  {
    id: 'line',
    name: 'LINE',
    supportLevel: 'Plugin',
    summary: 'LINE Messaging API bot integration.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'matrix',
    name: 'Matrix',
    supportLevel: 'Plugin',
    summary: 'Matrix protocol integration for rooms and direct messaging.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'mattermost',
    name: 'Mattermost',
    supportLevel: 'Plugin',
    summary: 'Bot API + WebSocket for channels, groups, and DMs.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'microsoft-teams',
    name: 'Microsoft Teams',
    supportLevel: 'Plugin',
    summary: 'Enterprise support track for Teams environments.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'nextcloud-talk',
    name: 'Nextcloud Talk',
    supportLevel: 'Plugin',
    summary: 'Self-hosted chat via Nextcloud Talk integration.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'nostr',
    name: 'Nostr',
    supportLevel: 'Plugin',
    summary: 'Decentralized encrypted DMs via NIP-04 and modern NIP flows.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'signal',
    name: 'Signal',
    supportLevel: 'Built-in',
    summary: 'Privacy-focused messaging through signal-cli.',
  },
  {
    id: 'synology-chat',
    name: 'Synology Chat',
    supportLevel: 'Plugin',
    summary: 'Synology NAS Chat via outgoing and incoming webhooks.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'slack',
    name: 'Slack',
    supportLevel: 'Built-in',
    summary: 'Slack workspace apps powered by Bolt SDK.',
  },
  {
    id: 'telegram',
    name: 'Telegram',
    supportLevel: 'Built-in',
    summary: 'Bot API integration via grammY with strong group support.',
  },
  {
    id: 'tlon',
    name: 'Tlon',
    supportLevel: 'Plugin',
    summary: 'Urbit-based messenger integration path.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'twitch',
    name: 'Twitch',
    supportLevel: 'Plugin',
    summary: 'Twitch chat support over IRC connection.',
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'webchat',
    name: 'WebChat',
    supportLevel: 'Built-in',
    summary: 'Gateway WebChat UI over WebSocket for browser-based sessions.',
  },
  {
    id: 'whatsapp',
    name: 'WhatsApp',
    supportLevel: 'Built-in',
    summary: 'Baileys-backed integration with QR pairing flow.',
  },
  {
    id: 'zalo',
    name: 'Zalo',
    supportLevel: 'Plugin',
    summary: "Zalo Bot API for Vietnam's popular messenger ecosystem.",
    details: 'Plugin track, installed separately.',
  },
  {
    id: 'zalo-personal',
    name: 'Zalo Personal',
    supportLevel: 'Plugin',
    summary: 'Personal account integration with QR login.',
    details: 'Plugin track, installed separately.',
  },
];

export const CHAT_CHANNEL_NOTES: string[] = [
  'Channels can run simultaneously; configure multiple and ZeroClaw routes per chat.',
  'Fastest initial setup is usually Telegram with a simple bot token.',
  'WhatsApp requires local state on disk for persistent sessions.',
  'Group behavior varies by channel. See docs/channels-reference.md for policy details.',
  'DM pairing and allowlists are enforced for safety. See docs/security/README.md.',
  'Troubleshooting lives in docs/troubleshooting.md under channel guidance.',
  'Model providers are documented separately in docs/providers-reference.md.',
];
