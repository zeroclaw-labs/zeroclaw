# Scheduled jobs (cron)

The scheduler inside `zeroclaw daemon` runs jobs on a schedule. A job is either
a shell command or an agent prompt, and every job runs as one configured agent:
that agent's risk profile gates the job's shell commands, and an agent job runs
as a turn of that agent.

SOPs have their own cron trigger, which the SOP maintenance tick drives instead
of this scheduler. See [SOP cron triggers](../sop/fan-in/cron.md).

## Scheduler settings

The `[scheduler]` section controls the loop itself:

| Field | Default | Effect |
|---|---:|---|
| `enabled` | `true` | When `false`, no cron jobs run. |
| `max_tasks` | `64` | Maximum stored jobs taken in one polling cycle. |
| `max_concurrent` | `4` | Maximum jobs executed in parallel within one polling cycle. |
| `catch_up_on_startup` | `true` | Jobs whose next run passed while the daemon was down fire once at startup. Set `false` to wait for the next scheduled occurrence instead. |
| `max_run_history` | `50` | Number of run records retained. |

## Declarative jobs in config

A `[cron.<id>]` entry defines a job in config. The map key is the job's stable
id. An agent claims the job by listing that id in its `cron_jobs`:

```toml
[cron.morning-digest]
name = "Morning digest"
job_type = "agent"
prompt = "Summarize yesterday's open issues and list the three most urgent."
schedule = { kind = "cron", expr = "0 9 * * 1-5", tz = "America/New_York" }
uses_memory = false
delivery = { mode = "announce", channel = "telegram.ops", to = "-1001234567890" }

[agents.digest]
cron_jobs = ["morning-digest"]
# ...the agent's model_provider, risk_profile and other settings
```

| Field | Default | Effect |
|---|---|---|
| `job_type` | `"shell"` | `"shell"` runs `command`; `"agent"` runs `prompt`. |
| `schedule` | (required) | `{ kind = "cron", expr = "...", tz = "..." }` for a five-field cron expression with an optional IANA timezone, `{ kind = "every", every_ms = 3600000 }` for a fixed interval, or `{ kind = "at", at = "2026-12-01T09:00:00Z" }` for one run at an RFC 3339 time. |
| `command` | | Shell command. Required for a shell job. |
| `prompt` | | Agent prompt. Required for an agent job. |
| `name` | | Human-readable name shown in listings and in the agent prompt prefix. |
| `enabled` | `true` | A disabled job stays defined but does not run. |
| `model` | | Model override for an agent job. See [Choosing the model](#choosing-the-model). |
| `allowed_tools` | | Tool allowlist for an agent job. See [Tools](#tools). |
| `uses_memory` | `true` | Whether an agent job recalls memory before it runs. See [Memory and sessions](#memory-and-sessions). |
| `session_target` | `"isolated"` | `"isolated"` or `"main"`. See [Memory and sessions](#memory-and-sessions). |
| `delivery` | none | Where to send the job's output. See [Delivery](#delivery). |
| `shell_output_format` | `"wrapped"` | Shell jobs only. See [Shell jobs](#shell-jobs). |

Exactly one enabled agent must claim each declarative job. A job that no
enabled agent claims, or that two enabled agents claim, is refused instead of
running under an arbitrary agent, and the scheduler logs that the job has no
single owning agent.

When the scheduler starts, it reconciles declarative jobs into its job store.
The config entry wins over the stored copy, and removing an entry from config
deletes that job and its run history. The reconciliation never removes jobs
created at runtime.

## Jobs created at runtime

The `zeroclaw cron` commands create and manage stored jobs. The commands that
create or update a job name its owning agent with `--agent`, which is required.
Without `--prompt`, the job argument is a shell command:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw cron add '0 9 * * 1-5' 'Summarize open issues' --agent digest --prompt --tz America/New_York
zeroclaw cron add-every 3600000 'df -h /' --agent ops
zeroclaw cron once 30m 'Check the deploy finished' --agent ops --prompt
zeroclaw cron add-at 2026-12-01T09:00:00Z 'Send the monthly report' --agent digest --prompt
zeroclaw cron list
zeroclaw cron update <id> --agent digest --expression '0 8 * * 1-5'
zeroclaw cron pause <id>
zeroclaw cron resume <id>
zeroclaw cron remove <id>
```

</div>

Prompt jobs also accept `--allowed-tool` (repeatable) and `--uses-memory false`.
`--channel`, `--to`, `--thread`, and `--no-best-effort` configure delivery. The
[CLI reference](../reference/cli.md) lists every flag.

Agents can schedule jobs with the `cron_add` and `cron_update` tools, and the
gateway's `POST /api/cron` endpoint, which the dashboard uses, creates them over
HTTP. The tools and the endpoint accept a `model` override. The CLI has no
`--model` flag yet, so set a job's model in config, through a tool, or with the
endpoint.

## Agent jobs

An agent job runs its prompt as a turn of the owning agent. The prompt is
prefixed with `[cron:<id> <name>]` so the agent can tell a scheduled run from a
conversation.

### Choosing the model

By default, an agent job uses the owning agent's model provider and model. Set
`model` to run the job on another model from that **same** provider: the
override replaces the model name only, and the provider stays the agent's
`model_provider`. Setting `model` also skips the agent's automatic effort
routing for that run.

To run a job on a different provider, such as a free local model for a
low-priority digest, configure a small agent whose `model_provider` points at
that provider and make it the job's owner.

### Tools

When `allowed_tools` is unset, the job gets the agent's usual tools except the
scheduler's own (`cron_add`, `cron_update`, `cron_remove`, `cron_run`, and
`schedule`), so a job cannot reschedule itself. Setting `allowed_tools`
restricts the run to the listed tools and replaces that default exclusion, so
list a scheduler tool only when the job should manage jobs.

### Memory and sessions

With `uses_memory = true`, the run recalls memory context before it starts. With
`uses_memory = false`, the run is memory-free: nothing is recalled, and the
memory tools are removed. Use it for stateless digest and report jobs. Cron
runs never auto-save their conversation to memory.

`session_target = "isolated"` gives each run its own new session.
`session_target = "main"` runs every execution in one shared `main` session.

## Shell jobs

A shell job runs `command` under the owning agent's risk profile, so a command
that profile does not allow is refused.

`shell_output_format` controls what the job returns:

- `"wrapped"` (default) returns stdout and stderr in a `status=... / stdout: /
  stderr:` envelope.
- `"raw"` returns trimmed stdout when the process exits zero. A non-zero exit
  still returns the wrapped envelope for diagnosis.

In both formats, a failure that never reaches a process exit (a security-policy
denial, a spawn failure, or a timeout) returns a plain error string. Setting
`shell_output_format` on an agent job is a configuration error.

## Delivery

Without `delivery`, or with `mode = "none"`, the output stays in the run
history. With `mode = "announce"`, the output is sent to a channel:

| Field | Effect |
|---|---|
| `channel` | Channel as `<type>.<alias>`, for example `"telegram.ops"`. A bare type such as `"telegram"` resolves only while exactly one instance of that type is configured, so prefer the aliased form. |
| `to` | Recipient on that channel, such as a Telegram chat id. |
| `thread_id` | Optional thread or conversation id, for channels that route on one. |
| `best_effort` | Default `true`: a failed delivery does not fail the job. Set `false` to mark the run failed when delivery fails. |

## When a job does not run

- Confirm `zeroclaw daemon` is running and `[scheduler] enabled` is not `false`.
- For a declarative job, confirm exactly one **enabled** agent lists its id in
  `cron_jobs`. The daemon log names jobs refused for having no single owner.
- Confirm the job has the `command` or `prompt` its `job_type` requires. An
  invalid declarative entry stops the reconciliation, which the scheduler logs
  as a warning.
