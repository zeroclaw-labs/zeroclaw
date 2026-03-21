# Matrix Server Migration

Guide for migrating the Matrix channel from `dustinllm.local` to a new host
(`matrix.local` or any new server name). Also covers architecture decisions
made during the planning phase.

## Background

Matrix room IDs and user IDs are permanently bound to the homeserver name
(`!roomid:servername`, `@user:servername`). Changing the server name means
new room IDs — there is no in-place rename. A migration is a fresh start
with new accounts and rooms.

## Architecture Decisions

### Single bot vs per-project bots

The current schema supports one bot identity per zeroclaw instance
(`[channels_config.matrix]` is a single struct with one `access_token`).

**Decision: stay with one bot (`@zeroclaw`), keep `cron-bot` as a separate
identity for async/scheduled notifications.**

Rationale:
- Project context is already clear from which room a message arrives in.
- Per-project bots would require a schema change (`Vec<MatrixConfig>`) and
  spawning multiple Matrix connections with no meaningful functional gain.
- `cron-bot` as a second identity lets you visually distinguish scheduled
  posts from live conversation — worth keeping separate.

| | Single bot | Per-project bots |
|---|---|---|
| Setup | One account | N accounts + schema change |
| Isolation | Shared session | Separate sessions |
| Failure blast radius | All rooms | Per-bot |
| Visual distinction | Room name | Bot name |

### Matrix Spaces

Use a single **Projects** space to group all project rooms in the client UI.
Each project gets one room (not a sub-space) — the cron-bot posts serve as
the notification layer naturally.

```
📁 Projects (space)
├── zeroclaw
├── ticket
├── imagellm
├── musicllm
├── habla-spanish
└── continuwuity
```

No functional impact on zeroclaw routing — it still sees flat room IDs.

### Idle detection: Stop hook vs tmux polling

Claude Code supports a `Stop` hook that fires when Claude finishes a
response (returns to the `❯` prompt). This is a push signal — more reliable
than polling tmux pane content.

**Recommended split:**
- **Stop hook** → "Claude just went idle" (push, zero polling overhead)
- **`idle` command / `cron-bot-status.sh`** → "is Claude currently busy?"
  (pull, used when you want to check state on demand or before queuing work)

Stop hook example (`~/.claude/settings.json`):

```json
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{"type": "command", "command": "~/.claude/hooks/notify-matrix.sh"}]
    }]
  }
}
```

`~/.claude/hooks/notify-matrix.sh`:

```bash
#!/usr/bin/env bash
input="$(cat)"
cwd="$(echo "$input" | python3 -c "import json,sys; print(json.load(sys.stdin).get('cwd',''))")"

case "$cwd" in
    */code/zeroclaw*)    room="!<zeroclaw-room>:matrix.local" ;;
    */code/ticket*)      room="!<ticket-room>:matrix.local" ;;
    */code/imagellm*)    room="!<imagellm-room>:matrix.local" ;;
    */code/musicllm*)    room="!<musicllm-room>:matrix.local" ;;
    */code/habla-spanish*) room="!<habla-room>:matrix.local" ;;
    *) exit 0 ;;
esac

# Post via cron-bot credentials
CRON_BOT_CONFIG="$HOME/.zeroclaw/cron-bot.json"
HOMESERVER="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['homeserver'])")"
ACCESS_TOKEN="$(python3 -c "import json; print(json.load(open('$CRON_BOT_CONFIG'))['access_token'])")"
ENCODED_ROOM="$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=''))" "$room")"
TXN="stop-hook-$(date -u +%s%3N)"
PROJECT="$(basename "$cwd")"

curl -sf -X PUT \
  "${HOMESERVER}/_matrix/client/v3/rooms/${ENCODED_ROOM}/send/m.room.message/${TXN}" \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"msgtype\":\"m.text\",\"body\":\"💤 Claude Code idle in \`${PROJECT}\`\"}" \
  > /dev/null
```

## Users to Create

| Account | Purpose |
|---|---|
| `@dustin:matrix.local` | Primary human user |
| `@zeroclaw:matrix.local` | Main bot (all project rooms) |
| `@cron-bot:matrix.local` | Scheduled post identity |
| `@zc-test:matrix.local` | Secondary allowed user for testing |

## Config Changes

Every config key containing a room ID must be updated. There are four
sections in `~/.zeroclaw/config.toml`:

### 1. Bot identity

```toml
[channels_config.matrix]
homeserver    = "http://matrix.local:6167"
access_token  = "<new-token>"
user_id       = "@zeroclaw:matrix.local"
device_id     = "<new-device-id>"
room_id       = "!<primary-room>:matrix.local"
room_ids      = []
allowed_users = ["@dustin:matrix.local"]
```

### 2. Workspace mappings (keyed by room ID)

```toml
[channel_workspaces]
"!<zeroclaw-room>:matrix.local"    = "/Users/dustin/code/zeroclaw"
"!<ticket-room>:matrix.local"      = "/Users/dustin/code/ticket"
"!<imagellm-room>:matrix.local"    = "/Users/dustin/code/imagellm"
"!<musicllm-room>:matrix.local"    = "/Users/dustin/code/musicllm"
"!<habla-room>:matrix.local"       = "/Users/dustin/code/habla-spanish"
"!<continuwuity-room>:matrix.local" = "/Users/dustin/code/continuwuity"
"!<primary-room>:matrix.local"     = "/Users/dustin"
```

### 3. Tmux targets (keyed by room ID)

```toml
[tmux_targets]
"!<zeroclaw-room>:matrix.local"  = "main:zeroclaw"
"!<ticket-room>:matrix.local"    = "main:ticket"
"!<imagellm-room>:matrix.local"  = "main:imagellm"
"!<musicllm-room>:matrix.local"  = "main:musicllm"
"!<habla-room>:matrix.local"     = "main:habla-spanish"
```

### 4. Per-room provider overrides

```toml
[channel_providers."!<musicllm-room>:matrix.local"]
provider = "claude-code"
model    = "opus"

[channel_providers."!<primary-room>:matrix.local"]
provider = "ollama"
model    = "qwen3:4b"
```

### 5. cron-bot credentials

Update `~/.zeroclaw/cron-bot.json`:

```json
{
  "homeserver":   "http://matrix.local:6167",
  "access_token": "<cron-bot-token>",
  "user_id":      "@cron-bot:matrix.local"
}
```

### 6. Cron job room IDs

Re-run `dev/setup-room-cron.sh` with the new room IDs after updating config,
or manually update the crontab entries that pass room IDs to
`services/cron-bot-triage.sh` and `services/cron-bot-status.sh`.

## Migration Checklist

- [ ] Install/run continuwuity on `matrix.local`
- [ ] Register users: `dustin`, `zeroclaw`, `cron-bot`, `zc-test`
- [ ] Create a Projects space; create one room per project; note room IDs
- [ ] Update `~/.zeroclaw/config.toml` (all four sections above)
- [ ] Update `~/.zeroclaw/cron-bot.json`
- [ ] Re-run `dev/setup-room-cron.sh` to update crontab
- [ ] Add Stop hook to `~/.claude/settings.json`
- [ ] Write `~/.claude/hooks/notify-matrix.sh` with new room IDs
- [ ] `zeroclaw daemon` — verify Matrix connects and lists expected rooms
- [ ] Send `idle` in each room — verify tmux targets resolve correctly
- [ ] Send `cron` in a room — verify workspace and job listing work
- [ ] Trigger a cron-bot triage run manually to verify credentials

## Helper: add tmux target after migration

```bash
# After updating config with new room IDs:
./dev/add-tmux-target.sh '!<new-room-id>:matrix.local' 'main:projectname'
./dev/add-tmux-target.sh --list   # verify
```
