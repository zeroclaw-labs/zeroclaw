# Zulip

Native client for a Zulip organization. The bot receives events through a Zulip
event queue (`POST /api/v1/register`, then long-polling `GET /api/v1/events`) and
sends with the REST API, authenticated with HTTP Basic (bot email + API key).
It handles direct messages, group direct messages and stream topics, tool
approvals, and approval prompts for parked SOP runs.

Requires **Zulip 8.0 or later** (feature level 215). On an older server the
listener refuses to start with an error naming the required version.

## Who can talk to the agent

{{#peer-group zulip}}

Zulip identities are **numeric user IDs**, not emails or display names. Find a
user's ID on their profile card (**User ID**), or in the organization's user list
when you are an administrator.

## Quickstart

1. In Zulip, open the **Bots** settings page and **Add a new bot** of type
   **Generic bot**. Note its email and download its `zuliprc`, which holds the
   API key.
2. Subscribe the bot to the streams it should read. A bot only receives stream
   messages from streams it is subscribed to.
3. Configure the channel:

```toml
[channels.zulip.main]
enabled = true
site = "https://zulip.example.com"
email = "zeroclaw-bot@zulip.example.com"
# api_key is a secret: set it through `zeroclaw config set` or the gateway
stream_ids = ["3", "5"]
mention_only = true

[agents.primary]
channels = ["zulip.main"]

[peer_groups.ops]
channel = "zulip.main"
agents = ["primary"]
external_peers = ["10", "11"]
```

`api_key` is a secret:

{{#secret-config channels.zulip.<alias>.api_key}}

### Field reference

{{#config-fields channels.zulip}}

## Conversations

| Where the message is | `reply_target` | Needs a mention with `mention_only = true` |
|---|---|---|
| Stream topic | `stream:<stream_id>:<topic>` | yes |
| Direct message | `dm:<user_id>` | no |
| Group direct message including the bot | `dm:<id>,<id>` (sorted, without the bot) | no |

`stream_ids` restricts the bot to the listed streams; empty means every stream
the bot is subscribed to. Direct messages always bypass `mention_only`: a private
conversation has no ambient noise to filter.

Direct messages the bot is **not a recipient of** are ignored, even when the bot
is mentioned in them. Zulip can deliver such messages to a mentioned bot; they
belong to other people's conversations and never reach the agent.

Messages from senders outside the alias's peer group are dropped and logged at
`DEBUG`.

## Private certificate authorities

For a server whose certificate is signed by an internal CA, point
`tls_ca_cert_path` at the CA certificate in PEM form. It is added to the built-in
web PKI roots for this channel only. A missing or invalid file is reported at
startup and the alias is skipped.

## Tool approval over chat (`approval_timeout_secs`)

When a tool needs approval, the agent posts the request in the conversation the
message came from:

```
APPROVAL REQUIRED [a1b2c3]
Tool: shell
Args: command: pwd
Reply: "a1b2c3 yes", "a1b2c3 no", or "a1b2c3 always"
```

Answer it in either way:

- **React** with ✅ (`check`) to approve or ❌ (`cross_mark`) to deny. The bot
  seeds both reactions on the prompt. `always` has no reaction on purpose: it
  widens the session's permissions and stays a typed decision.
- **Reply** with the token, e.g. `a1b2c3 yes`. The reply is consumed and never
  reaches the model.

Only peer-group members can answer, and only in the conversation the prompt was
posted to. Once decided, the prompt is edited to show the outcome (approved,
denied, or expired). A bare `yes` or `no` without the token, sent while a prompt
is pending in the same conversation, gets an immediate reminder with the token
instead of waiting in the agent's queue.

`approval_timeout_secs` bounds the wait. The default is 300 seconds; `0` denies
immediately. A timeout is recorded as the runtime denying, not as a person.

## SOP approval gates

The channel implements gate prompts, so a parked SOP run can ask for approval in
a Zulip stream or direct message. Point the approval policy's route at the
channel; the recipient uses the `reply_target` format:

```toml
[sop.approval.groups.release]
members = ["channel:zulip.main:10"]

[sop.approval.policies.prod]
required_group = "release"
request_route = "zulip.main:stream:5:approvals"
```

The prompt shows the title, what is being approved (the step's `- prompt:` when
the SOP has one) and the ✅/❌ reactions. The text-reply form and a marker line
are folded in a collapsed **Details** spoiler:

```
`zeroclaw-gate:<reference>:<signature>`
```

The marker binds the message to the run without in-memory state, so reactions
keep working after a daemon restart. The signature is an HMAC-SHA256 over the
reference and the place the prompt was posted to, keyed from the bot's API key.
A marker the agent is talked into writing in an ordinary reply is not valid:
outgoing text is sanitized so that only the gate prompt itself can carry one, and
a marker moved to another place or forged without the key is rejected.

Answers are also accepted as text: `approve <reference>` or `deny <reference>`
in the same place.

Limits:

- Edit and Revise need a form, which Zulip does not have: they are not offered as
  reactions and stay answerable from channels that support them.
- After a restart, in a stream with `mention_only`, a text answer must mention
  the bot; reactions always work.
- Rotating the bot's API key invalidates the signature of prompts already posted:
  their reactions are ignored, the text answer still works.
- If the organization limits how long messages can be edited, the outcome of a
  gate decided after that window is posted as a new reply instead of editing the
  prompt.

## Several bots

Each `[channels.zulip.<alias>]` has its own event queue and is bound to one
agent; peer groups and approval routes can target a single alias
(`zulip.<alias>`). Two layouts work:

- **One bot per agent**: every alias has its own bot credentials.
- **One bot for several agents**: aliases share the same bot credentials, each
  with its own `stream_ids`, so the stream decides which agent answers. Set
  `dm_enabled = false` on these aliases, otherwise every alias of the bot would
  answer the same direct message. Each alias also accepts gate reactions only on
  prompts posted in its own streams.

```toml
[channels.zulip.prj_a]
email = "projects-bot@zulip.example.com"
stream_ids = ["11"]
dm_enabled = false

[channels.zulip.prj_b]
email = "projects-bot@zulip.example.com"
stream_ids = ["12"]
dm_enabled = false
```

## Operational notes

1. One long-poll connection per alias. Zulip answers at least once a minute with
   a heartbeat.
2. An expired event queue is registered again automatically. Events that arrive
   between the expiry and the new registration are **not** replayed; for a chat
   this is acceptable, but a message sent in that window gets no answer.
3. Rate limits (`429`) are honoured with `Retry-After`. Transient failures (5xx,
   a non-JSON error page from a proxy, timeouts) are retried up to five times
   with exponential backoff while keeping the queue; past that, the listener
   exits and the channel supervisor restarts it.
4. The bot identity is fetched once via `GET /api/v1/users/me` and cached for the
   process lifetime.
5. Attachments are not uploaded yet: outgoing messages carry text only.

## See also

- [Channels overview](./overview.md)
- [Peer Groups](./peer-groups.md)
- [Reference: config schema](../reference/config.md)
