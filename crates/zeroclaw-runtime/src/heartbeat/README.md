# Deadman notification lifecycle

The daemon owns the heartbeat worker and its watchdog in one selected future.
Dropping/reloading the parent drops both and any pending delivery future. An
already accepted channel message cannot be recalled by cancellation.

Each generation first awaits one watchdog check before polling the worker. Its
channel attempt is bounded to 30 seconds. This prevents repeated immediate
worker failures from resetting the first-check delay or systematically canceling
an overdue notification before the asynchronous channel call can finish. It can
delay worker startup by up to 30 seconds only for an overdue, unclaimed incident.
A daemon shutdown/reload can still cancel this initial check.

## Notification policy

The existing daemon live `Config` handle is the source for timeout, quiet hours,
and delivery route. The watcher resolves it every check, rather than retaining
a startup policy snapshot. Changes published through RPC/gateway config apply on
the next check (at most 60 seconds); CLI/file-only writes require normal daemon
config reload. An in-flight channel request may already have been accepted.

`heartbeat.deadman_timeout_minutes = 0` is the supported reversible owner mute
for this notification. It preserves ordinary heartbeat work and internal health
logging. Unmuting does not re-arm an already claimed incident. There is no
separate per-incident acknowledgement command.

An optional daily quiet window applies only to deadman notification attempts:

```toml
[heartbeat.deadman_quiet_hours]
start = "22:00"
end = "07:00"
timezone = "America/Los_Angeles"
```

Times must be exactly `HH:MM`, with an explicit IANA timezone. Start is inclusive
and end is exclusive; an earlier end wraps across midnight. Equal endpoints are
rejected; use timeout zero for a full mute. Daylight-saving transitions use local
wall time, including both instances of a repeated hour. Omit the table to allow
notifications at all hours. Save the complete table together through config
patch/editor when introducing it, so intermediate incomplete values are not
applied.

Config saves reject invalid quiet windows before persistence or live publication.
If malformed policy arrives through a manually edited file or a direct runtime
handle, notification checks report `heartbeat-notification-policy` unhealthy
and log the error. They suppress only this notification while ordinary heartbeat
work continues. Correcting, removing, or muting the policy clears that health
error at the next check.

Quiet hours defer without claiming an incident. At the next allowed check, only
an incident that is still overdue may notify. A heartbeat that recovers during
the quiet window does not emit a stale alert when the window ends. Ordinary
heartbeat tasks, their delivery policy, and other failure alerts are unchanged.

## Incident and delivery evidence

The `heartbeat_deadman` row in the existing
`<data_dir>/heartbeat/history.db` owns the monitoring baseline, completed-tick
generation, and most recent notification attempt. Live metrics and per-task
history are observational and cannot settle/re-arm an incident. First startup
records a baseline so failure to complete any initial tick is monitored too.
Reload/restart preserves that baseline rather than postponing the deadline.

After the configured interval is exceeded, a SQLite update atomically claims
the current tick generation and commits `unknown` with full synchronization
before calling the channel. Repeated checks, concurrent connections, timeout,
cancellation, and process death cannot claim that same generation again. Alerts
include the incident generation and instructions for the narrow mute.

The deadman delivery path requires a registered handler; an absent handler is an
error and cannot record a false delivery success. A successful callback records
`delivered`, meaning the channel adapter acknowledged the operation. The current
channel contract returns no destination message ID, so this is not a separate
readback receipt. Failure or timeout retains `unknown`, without automatic retry.
A crash between claim and send may suppress an alert that was never sent;
avoiding duplicate uncertain delivery takes precedence over guaranteed delivery.

Only an actual completed worker tick re-arms monitoring, including completed
failed ticks, empty task lists, and a two-phase decision to skip. The first tick
after an incident records one internal recovery log; it sends no external
recovery notification. Persistence errors propagate to the heartbeat supervisor;
no alert is sent without a committed claim.

Tests exercise real SQLite files and competing connections, a child process
that exits after claiming but before saving delivery outcome, fast worker
failures, bounded asynchronous startup delivery, virtual-clock cancellation and
mute changes, quiet-hour deferral and recovery, DST, and strict delivery-handler
outcomes. No real channel send is needed for these checks.

The table is additive and existing task history is preserved. Older binaries
ignore the table and retain the previous repeated-alert behavior. Set the narrow
timeout mute before downgrading; keep the database and newer delivery claims.
Deleting watchdog state to force a retry can repeat an uncertain delivery.
