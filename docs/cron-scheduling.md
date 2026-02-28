# Cron & Scheduling System

ZeroClaw includes a full-featured job scheduling system for running tasks on a schedule, at specific times, or at regular intervals.

## Quick Start

```bash
# Add a cron job (runs every day at 9 AM)
zeroclaw cron add '0 9 * * *' 'echo "Good morning!"'

# Add a one-shot reminder (runs in 30 minutes)
zeroclaw cron once 30m 'notify-send "Time is up!"'

# Add an interval job (runs every 5 minutes)
zeroclaw cron add-every 300000 'curl -s http://api.example.com/health'

# List all jobs
zeroclaw cron list

# Remove a job
zeroclaw cron remove <job-id>
```

## Schedule Types

### Cron Expressions (`kind: "cron"`)

Standard cron expressions with optional timezone support.

```bash
# Every weekday at 9 AM Pacific
zeroclaw cron add '0 9 * * 1-5' --tz 'America/Los_Angeles' 'echo "Work time"'

# Every hour
zeroclaw cron add '0 * * * *' 'echo "Hourly check"'

# Every 15 minutes
zeroclaw cron add '*/15 * * * *' 'curl http://localhost:8080/ping'
```

**Format:** `minute hour day-of-month month day-of-week`

| Field | Values |
|-------|--------|
| minute | 0-59 |
| hour | 0-23 |
| day-of-month | 1-31 |
| month | 1-12 |
| day-of-week | 0-6 (Sun-Sat) |

### One-Shot (`kind: "at"`)

Run exactly once at a specific time.

```bash
# At a specific ISO timestamp
zeroclaw cron add-at '2026-03-15T14:30:00Z' 'echo "Meeting starts!"'

# Relative delay (human-friendly)
zeroclaw cron once 2h 'echo "Two hours later"'
zeroclaw cron once 30m 'echo "Half hour reminder"'
zeroclaw cron once 1d 'echo "Tomorrow"'
```

**Delay units:** `s` (seconds), `m` (minutes), `h` (hours), `d` (days)

### Interval (`kind: "every"`)

Run repeatedly at a fixed interval.

```bash
# Every 5 minutes (300000 ms)
zeroclaw cron add-every 300000 'echo "Ping"'

# Every hour (3600000 ms)
zeroclaw cron add-every 3600000 'curl http://api.example.com/sync'
```

## Job Types

### Shell Jobs

Execute shell commands directly:

```bash
zeroclaw cron add '0 6 * * *' 'backup.sh && notify-send "Backup done"'
```

### Agent Jobs

Send prompts to the AI agent:

```toml
# In zeroclaw.toml
[[cron.jobs]]
schedule = { kind = "cron", expr = "0 9 * * *", tz = "America/Los_Angeles" }
job_type = "agent"
prompt = "Check my calendar and summarize today's events"
session_target = "main"  # or "isolated"
```

## Session Targeting

Control where agent jobs run:

| Target | Behavior |
|--------|----------|
| `isolated` (default) | Spawns new session, no history |
| `main` | Runs in main session with full context |

```toml
[[cron.jobs]]
schedule = { kind = "every", every_ms = 1800000 }  # 30 min
job_type = "agent"
prompt = "Check for new emails and summarize any urgent ones"
session_target = "main"  # Has access to conversation history
```

## Delivery Configuration

Route job output to channels:

```toml
[[cron.jobs]]
schedule = { kind = "cron", expr = "0 8 * * *" }
job_type = "agent"
prompt = "Generate a morning briefing"
session_target = "isolated"

[cron.jobs.delivery]
mode = "channel"
channel = "telegram"
to = "123456789"  # Telegram chat ID
best_effort = true  # Don't fail if delivery fails
```

**Delivery modes:**
- `none` - No output delivery (default)
- `channel` - Send to a specific channel
- `notify` - System notification

## CLI Commands

| Command | Description |
|---------|-------------|
| `zeroclaw cron list` | Show all scheduled jobs |
| `zeroclaw cron add <expr> <cmd>` | Add cron-expression job |
| `zeroclaw cron add-at <time> <cmd>` | Add one-shot at time |
| `zeroclaw cron add-every <ms> <cmd>` | Add interval job |
| `zeroclaw cron once <delay> <cmd>` | Add one-shot with delay |
| `zeroclaw cron update <id> [opts]` | Update job settings |
| `zeroclaw cron remove <id>` | Delete a job |
| `zeroclaw cron pause <id>` | Pause (disable) job |
| `zeroclaw cron resume <id>` | Resume (enable) job |

## Configuration File

Define jobs in `zeroclaw.toml`:

```toml
[[cron.jobs]]
name = "morning-briefing"
schedule = { kind = "cron", expr = "0 8 * * 1-5", tz = "America/New_York" }
job_type = "agent"
prompt = "Good morning! Check my calendar, emails, and weather."
session_target = "main"
enabled = true

[[cron.jobs]]
name = "health-check"
schedule = { kind = "every", every_ms = 60000 }
job_type = "shell"
command = "curl -sf http://localhost:8080/health || notify-send 'Service down!'"
enabled = true

[[cron.jobs]]
name = "daily-backup"
schedule = { kind = "cron", expr = "0 2 * * *" }
job_type = "shell"
command = "/home/user/scripts/backup.sh"
enabled = true
```

## Tool Integration

The cron system is also available as agent tools:

| Tool | Description |
|------|-------------|
| `cron_add` | Create a new cron job |
| `cron_list` | List all jobs |
| `cron_remove` | Delete a job |
| `cron_update` | Modify a job |
| `cron_run` | Force-run a job immediately |
| `cron_runs` | Show recent run history |

### Example: Agent creating a reminder

```
User: Remind me to call mom in 2 hours
Agent: [uses cron_add with kind="at" and delay="2h"]
Done! I'll remind you to call mom at 4:30 PM.
```

## Migration from OpenClaw

ZeroClaw's cron system is compatible with OpenClaw's scheduling:

| OpenClaw | ZeroClaw |
|----------|----------|
| `kind: "cron"` | `kind = "cron"` ✅ |
| `kind: "every"` | `kind = "every"` ✅ |
| `kind: "at"` | `kind = "at"` ✅ |
| `sessionTarget: "main"` | `session_target = "main"` ✅ |
| `sessionTarget: "isolated"` | `session_target = "isolated"` ✅ |
| `payload.kind: "systemEvent"` | `job_type = "agent"` |
| `payload.kind: "agentTurn"` | `job_type = "agent"` |

**Key difference:** ZeroClaw uses TOML config format, OpenClaw uses JSON.

## Best Practices

1. **Use timezones** for user-facing schedules (meetings, reminders)
2. **Use intervals** for background tasks (health checks, syncs)
3. **Use one-shots** for reminders and delayed actions
4. **Set `session_target = "main"`** when the agent needs conversation context
5. **Use `delivery`** to route output to the right channel

## Troubleshooting

**Job not running?**
- Check `zeroclaw cron list` - is it enabled?
- Verify the cron expression is correct
- Check timezone settings

**Agent job has no context?**
- Change `session_target` from `"isolated"` to `"main"`

**Output not delivered?**
- Verify `delivery.channel` is configured
- Check that the target channel is active
