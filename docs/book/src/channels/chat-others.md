# Other Chat Platforms

Channels with working integrations but not yet pulled out into dedicated guides. Each TOML block is schema-derived; for the full field index and defaults, see the [Config reference](../reference/config.md).

## Discord

```toml
[channels.discord]
enabled = true
bot_token = "..."                          # from https://discord.com/developers/applications
guild_id = "1234567890"                    # optional: restrict the bot to a single guild
allowed_users = ["111111111111111111"]     # empty = deny all
listen_to_bots = false                     # process messages from other bots
interrupt_on_new_message = false           # cancel in-flight reply when a newer message arrives
mention_only = false                       # only respond when the bot is @-mentioned
stream_mode = "off"                        # "off" (default), "partial", or "multi_message"
draft_update_interval_ms = 750             # bump if hitting Discord rate limits
multi_message_delay_ms = 0                 # delay between paragraph chunks in multi_message mode
stall_timeout_secs = 0                     # 0 = stall watchdog disabled
proxy_url = ""
```

- **Bot intents needed:** Message Content Intent, Server Members Intent. Set in the Developer Portal.
- **Streaming:** off / partial (edit-in-place draft) / multi_message (paragraph-split).
- **Tool-call indicator:** typing indicator while tools run; visible code-block preview for shell and browser calls.

## Slack

```toml
[channels.slack]
enabled = true
bot_token = "xoxb-..."                     # bot OAuth token
app_token = "xapp-..."                     # optional: app-level token for Socket Mode
channel_ids = ["C01..."]                   # empty = listen on every accessible channel
allowed_users = ["U01..."]                 # empty = deny all
thread_replies = true                      # default true: replies stay in the originating thread
mention_only = false                       # only respond when @-mentioned in groups
strict_mention_in_thread = false           # require @-mention even inside an active thread
use_markdown_blocks = false                # newer "markdown" block type (12 000 char limit)
stream_drafts = false                      # progressive chat.update edits
draft_update_interval_ms = 1200            # default 1200 (Slack rate-limit-friendly)
cancel_reaction = "x"                      # emoji name (no colons) that cancels in-flight tasks
interrupt_on_new_message = false
proxy_url = ""
```

- **Socket Mode** is enabled by setting `app_token`. Without `app_token`, point Slack's Events API at the gateway's HTTP endpoint instead.
- Supports threaded replies, mention gating, draft streaming, and reaction-based cancellation.

## Telegram

```toml
[channels.telegram]
enabled = true
bot_token = "..."                          # from @BotFather
allowed_users = ["123456789"]              # empty = deny all (string IDs or usernames)
mention_only = false                       # only respond when @-mentioned in groups; DMs always pass
interrupt_on_new_message = false
stream_mode = "off"                        # "off" (default), "partial", or "multi_message"
draft_update_interval_ms = 750             # bump if "Too Many Requests"
ack_reactions = true                       # optional bool override of [channels].ack_reactions
approval_timeout_secs = 120                # default 120 (inline-keyboard tool approval)
proxy_url = ""
```

The Telegram channel uses the Bot API's long-polling getUpdates loop — no public webhook URL is required.

## Signal

```toml
[channels.signal]
enabled = true
http_url = "http://127.0.0.1:8686"         # signal-cli daemon HTTP endpoint
account = "+14155550123"                   # E.164 phone number registered with signal-cli
group_id = ""                              # "" = all sources, "dm" = DMs only, or a specific group ID
allowed_from = ["+14155551234"]            # E.164 senders, or "*" for all
ignore_attachments = false                 # skip messages that are attachment-only (no text)
ignore_stories = false                     # skip incoming story messages
proxy_url = ""
```

Signal has no official bot API, so ZeroClaw drives the [signal-cli](https://github.com/AsamK/signal-cli) HTTP daemon. Run it locally and point `http_url` at it.

## iMessage (macOS only)

```toml
[channels.imessage]
enabled = true
allowed_contacts = ["+14155551234", "alice@icloud.com"]   # phone numbers or email addresses; empty = deny all
```

**macOS-only.** ZeroClaw reads the local Messages.app SQLite store and uses AppleScript automation to send replies — requires Full Disk Access and Accessibility grants for the daemon.

## WeCom (企业微信)

```toml
[channels.wecom]
enabled = true
webhook_key = "..."                        # key from the WeCom Bot webhook URL
allowed_users = ["UserID"]                 # empty = deny all, "*" = allow all
```

Chinese enterprise WeChat. The schema is intentionally minimal — WeCom's Bot Webhook is push-only, so the channel posts via the webhook key.

## DingTalk

```toml
[channels.dingtalk]
enabled = true
client_id = "..."                          # AppKey from the DingTalk developer console
client_secret = "..."                      # AppSecret
allowed_users = ["staff-id-1"]             # empty = deny all, "*" = allow all
proxy_url = ""
```

Alibaba's enterprise messenger. Uses DingTalk Stream Mode (long-running connection, no public URL needed).

## Lark / Feishu

```toml
[channels.lark]
enabled = true
app_id = "..."
app_secret = "..."
encrypt_key = ""                           # optional: webhook payload decryption key
verification_token = ""                    # optional: webhook validation token
use_feishu = false                         # true = use the Feishu (Chinese) endpoint
receive_mode = "websocket"                 # "websocket" (default, no public URL) or "webhook"
port = 0                                   # required when receive_mode = "webhook"; ignored otherwise
allowed_users = ["..."]                    # user IDs or union IDs; empty = deny all, "*" = allow all
mention_only = false                       # only respond when @-mentioned in groups
proxy_url = ""
```

Lark and Feishu share the same `LarkConfig`. Set `use_feishu = true` to switch to the Chinese endpoint.

## QQ

```toml
[channels.qq]
enabled = true
app_id = "..."                             # from the QQ Bot developer console
app_secret = "..."
allowed_users = ["..."]                    # empty = deny all, "*" = allow all
proxy_url = ""
```

Tencent's consumer messenger. Bot API access requires developer registration.

## IRC

```toml
[channels.irc]
enabled = true
server = "irc.libera.chat"
port = 6697                                # default 6697 (TLS)
nickname = "zeroclaw"
username = ""                              # defaults to nickname when unset
channels = ["#mychannel"]
allowed_users = ["alice"]                  # nicknames (case-insensitive); "*" for all
server_password = ""                       # bouncer password (e.g. ZNC)
nickserv_password = ""                     # NickServ IDENTIFY password
sasl_password = ""                         # SASL PLAIN password (IRCv3)
verify_tls = true                          # default true
mention_only = false
```

Classic IRC. Supports SASL, NickServ auth, and multiple channels.

## Mochat

```toml
[channels.mochat]
enabled = true
api_url = "https://mochat.example.com"
api_token = "..."
allowed_users = ["..."]                    # empty = deny all, "*" = allow all
poll_interval_secs = 5                     # default 5
```

Mochat customer-service API. Polled every `poll_interval_secs`.

## Notion

Notion is configured as a top-level integration, not a channel — the section header is `[notion]`, not `[channels.notion]`. It exposes a Notion database as a task surface (the agent polls for pending rows and writes results back).

```toml
[notion]
enabled = true
api_key = "..."                            # or set NOTION_API_KEY env var
database_id = "..."
poll_interval_secs = 5                     # default 5
status_property = "Status"                 # default "Status"
input_property = "Input"                   # default "Input"
result_property = "Result"                 # default "Result"
max_concurrent = 4                         # default 4 (concurrent in-flight tasks)
recover_stale = true                       # default true: pick up rows left mid-run
```

Useful for asynchronous workflows where the "channel" is a task inbox.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
