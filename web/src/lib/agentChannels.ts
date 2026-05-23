// Resolve an agent's bound channels and the room/username identity field
// of each. Consumed by the Cron UI: when an operator picks an agent for a
// new cron job, the delivery-channel select narrows to that agent's
// `agents.<alias>.channels` list with the matching identity info visible
// inline (matrix.<alias> shows the user_id, discord.<alias> shows the
// guild_ids, etc.) so the operator doesn't have to remember which
// composite goes where.
//
// Source of truth for the channel slot list is the schema's
// `ChannelsConfig` — this helper just walks the live config via the
// existing config endpoints. The per-channel-type identity field comes
// from a small per-type table below; adding a new channel family means
// one row here.

import { getProp } from './api';

export interface AgentBoundChannel {
  /** Composite `<type>.<alias>` as it appears in `agents.<alias>.channels`. */
  composite: string;
  /** Bare channel type (`matrix`, `discord`, ...). */
  type: string;
  /** Operator-chosen alias half of the composite. */
  alias: string;
  /** Short identity label shown next to the composite — room id, user
   *  id, guild id, etc. Empty when the channel is configured but its
   *  identity field is unset. */
  identity: string;
}

interface IdentityField {
  /** Field name under `[channels.<type>.<alias>]` that holds the
   *  room / username / guild identifier the operator recognises. */
  field: string;
  /** Optional label prefix shown in the UI (e.g. "room:", "guild:"). */
  label?: string;
}

// Per-channel-type identity field. Update this row when adding a new
// channel family — the actual channel slot list is in the schema's
// `ChannelsConfig` and the picker derives from there; this table only
// names which field to surface alongside the composite.
const CHANNEL_IDENTITY: Record<string, IdentityField> = {
  matrix: { field: 'user-id', label: 'user' },
  discord: { field: 'guild-ids', label: 'guilds' },
  slack: { field: 'channel-ids', label: 'channels' },
  mattermost: { field: 'channel-ids', label: 'channels' },
  telegram: { field: 'bot-token', label: '' }, // bot identity, no separate id
  signal: { field: 'user-id', label: 'user' },
  imessage: { field: 'user-id', label: 'user' },
  whatsapp: { field: 'user-id', label: 'user' },
  email: { field: 'address', label: '' },
  gmail_push: { field: 'address', label: '' },
  'gmail-push': { field: 'address', label: '' },
  irc: { field: 'nickname', label: 'nick' },
  nextcloud_talk: { field: 'user-id', label: 'user' },
  'nextcloud-talk': { field: 'user-id', label: 'user' },
};

/** Parse a config-encoded value into a short identity string. The
 *  `/api/config/prop` endpoint serialises arrays and strings as strings;
 *  we strip leading/trailing brackets / quotes / whitespace so the
 *  picker shows `matrix:@clamps-bot:matrix.org` instead of
 *  `matrix:"@clamps-bot:matrix.org"`. */
function shortIdentity(raw: unknown): string {
  if (typeof raw !== 'string') return '';
  const s = raw.trim();
  if (!s || s === '<unset>') return '';
  // Strip TOML-array wrapping.
  if (s.startsWith('[') && s.endsWith(']')) {
    return s
      .slice(1, -1)
      .split(',')
      .map((p) => p.trim().replace(/^"|"$/g, ''))
      .filter(Boolean)
      .join(', ');
  }
  return s.replace(/^"|"$/g, '');
}

/** Walk `agents.<alias>.channels` and resolve each composite's identity
 *  field. Channel composites that the agent lists but whose backing
 *  `[channels.<type>.<alias>]` entry doesn't exist yet still appear, with
 *  an empty identity — the operator can see they're dangling. */
export async function agentBoundChannels(
  agentAlias: string,
): Promise<AgentBoundChannel[]> {
  let raw: unknown;
  try {
    const r = await getProp(`agents.${agentAlias}.channels`);
    raw = r.value;
  } catch {
    return [];
  }
  let composites: string[];
  if (Array.isArray(raw)) {
    composites = raw.map(String);
  } else if (typeof raw === 'string' && raw && raw !== '<unset>') {
    // The `prop` endpoint sometimes returns the TOML-array string form.
    const trimmed = raw.trim();
    if (trimmed.startsWith('[') && trimmed.endsWith(']')) {
      composites = trimmed
        .slice(1, -1)
        .split(',')
        .map((p) => p.trim().replace(/^"|"$/g, ''))
        .filter(Boolean);
    } else {
      composites = [trimmed];
    }
  } else {
    composites = [];
  }

  const out: AgentBoundChannel[] = [];
  for (const composite of composites) {
    const dot = composite.indexOf('.');
    const [type, alias] =
      dot >= 0
        ? [composite.slice(0, dot), composite.slice(dot + 1)]
        : [composite, ''];
    const spec = CHANNEL_IDENTITY[type];
    let identity = '';
    if (spec && alias) {
      try {
        const r = await getProp(`channels.${type}.${alias}.${spec.field}`);
        identity = shortIdentity(r.value);
        if (identity && spec.label) {
          identity = `${spec.label}=${identity}`;
        }
      } catch {
        identity = '';
      }
    }
    out.push({ composite, type, alias, identity });
  }
  return out;
}
