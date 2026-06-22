# WeCom WebSocket

ZeroClaw supports WeCom AI Bot long-connection mode through
`[channels.wecom_ws.<alias>]`. The channel receives inbound messages over the
WeCom WebSocket subscription and sends active-session replies over the same
connection.

## Who can talk to the agent

{{#peer-group wecom_ws}}

WeCom WebSocket scopes use native WeCom IDs:

| Scope | Meaning |
|---|---|
| `user--<userid>` | A WeCom internal user ID. |
| `group--<chatid>` | A WeCom group chat ID. |
| `external--<external_userid>` | An external-contact user ID for the WeCom application message API. |

The `external--` prefix is required for external-contact proactive sends so the
channel can route through the HTTP application-message surface instead of the
WebSocket active-session reply path.

## Credentials

WeCom WebSocket uses two distinct credential pairs. Do not reuse one pair for
the other purpose.

| Field | Source | Used for |
|---|---|---|
| `bot_id` | WeCom AI Bot WebSocket subscription | Subscribing to `openws.work.weixin.qq.com`. |
| `secret` | WeCom AI Bot WebSocket subscription | Authenticating the WebSocket subscription. |
| `corp_id` | WeCom corp application | `qyapi` `gettoken`, proactive external sends, and media upload. |
| `corp_secret` | WeCom corp application | `qyapi` `gettoken`, proactive external sends, and media upload. |

`corp_id` and `corp_secret` are required only when the channel needs qyapi
application calls: proactive sends to `external--...` recipients or attachment
upload before a proactive send. If either value is missing, those paths fail
with a configuration error instead of falling back to `bot_id` / `secret`.

## Configuration

{{#config-fields channels.wecom_ws}}

Minimal example:

```toml
[channels.wecom_ws.work]
enabled = true
bot_id = "wecom-ai-bot-id"
secret = "wecom-ai-bot-subscription-secret"
corp_id = "wwxxxxxxxxxxxxxxxx"
corp_secret = "corp-application-secret"
bot_name = "zeroclaw"
```

Bind the channel to an agent through peer groups:

```toml
[peer_groups.wecom_work]
channel = "wecom_ws.work"
agents = ["default"]
external_peers = ["zhangsan", "group-chat-id"]
```

## Proactive Send

Use `zeroclaw channel send` for one-off messages. The `--channel-id` can be the
channel type (`wecom_ws`) or an aliased channel (`wecom_ws.work`), depending on
how the channel is configured.

Send to an internal user:

```sh
zeroclaw channel send 'Build succeeded.' --channel-id wecom_ws.work --recipient user--zhangsan
```

Send to a group:

```sh
zeroclaw channel send 'Nightly report is ready.' --channel-id wecom_ws.work --recipient group--group-chat-id
```

Send to an external contact through the WeCom application message API:

```sh
zeroclaw channel send 'Your support case has been updated.' --channel-id wecom_ws.work --recipient external--external-user-id
```

Internal user and group sends use the WebSocket `aibot_send_msg` surface.
External-contact sends use the qyapi HTTP `message/send` surface and therefore
require `corp_id` and `corp_secret`.

## Media

Inbound media attachments are downloaded and normalized when the incoming WeCom
payload includes a supported media URL and AES key. The channel handles image,
voice/audio, and file messages.

Proactive attachment sends upload media through qyapi `media/upload` before
sending the message. Supported upload wire types are:

| Attachment kind | WeCom upload `type` / `msgtype` |
|---|---|
| Image | `image` |
| Audio | `voice` |
| Video | `video` |
| Other file | `file` |

External-contact attachment sends are not currently supported; the channel
returns a clear error for attachments addressed to an `external--...` recipient.
