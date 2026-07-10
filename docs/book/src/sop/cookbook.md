# SOP Cookbook

Practical SOP templates in the runtime-supported `SOP.toml` + `SOP.md` format.

## 1. Human-in-the-Loop Deployment

```toml
[sop]
name = "deploy-prod"
description = "Production deployment with human approval gate"
execution_mode = "step_by_step"

[[triggers]]
type = "channel"
topic = "git.main:push"
```

The `SOP.md` body:

```md
## Steps

1. **Verify** — Check health metrics and rollout constraints.
   - tools: http_request

2. **Deploy** — Execute deployment command.
   - tools: shell
   - requires_confirmation: true
```

## 2. IoT Alert Handler (MQTT)

```toml
[sop]
name = "iot-alert"
description = "Handle IoT sensor alerts from MQTT"
priority = "high"

[[triggers]]
type = "mqtt"
topic = "sensors/temperature/#"
condition = "$.value > 85"
```

The `SOP.md` body:

```md
## Steps

1. **Analyze** — Read the `Payload:` section in this SOP context and determine severity.
   - tools: memory_recall

2. **Notify** — Send an alert with site/device/severity summary.
   - tools: pushover
```

## 3. Daily Digest (Cron)

```toml
[sop]
name = "daily-digest"
description = "Daily log digest and incident summary"
execution_mode = "auto"

[[triggers]]
type = "cron"
expression = "0 6 * * *"
```

The `SOP.md` body:

```md
## Steps

1. **Collect Logs** — Gather recent errors and warnings.
   - tools: file_read

2. **Summarize** — Produce concise incident and trend summary.
   - tools: memory_store
```

## 4. Filesystem Watch with Conditional Routing

```toml
[sop]
name = "config-drift"
description = "React to configuration file changes"
priority = "normal"

[[triggers]]
type = "filesystem"
path = "config/*.toml"
events = ["modified"]
condition = "$.changed > 0"
```

The `SOP.md` body:

```md
## Steps

1. **Diff** — Show what changed in the config file.
   - tools: shell

2. **Validate** — Run config validation against the updated file.
   - tools: shell
   - when: $.steps.1.ok == true

3. **Alert** — Notify the ops channel if validation fails.
   - tools: http_request
   - when: $.steps.2.ok == false
```

## 5. Deterministic Pipeline with Capability Steps

```toml
[sop]
name = "stagex-update"
description = "Deterministic auto-update pipeline"
deterministic = true
max_concurrent = 1

[[triggers]]
type = "amqp"
routing_key = "org.release.#"
```

The `SOP.md` body:

```md
## Steps

1. **Validate input** — Check the release payload schema.
   - kind: capability
   - capability: json.validate
   - with: {"schema":{"type":"object","required":["project","version"]}}

2. **Bump** — Set the new version and fetch source.
   - tools: shell, file_read, file_write
   - on_failure: retry:2

3. **Build** — Build the package and verify reproducibility.
   - tools: shell
   - on_failure: goto:5

4. **Commit + push** — Commit on a per-package branch and push.
   - tools: shell
   - kind: checkpoint

5. **Report failure** — Post the failure outcome.
   - tools: http_request
```

## 6. AMQP Release Feed

```toml
[sop]
name = "release-monitor"
description = "Monitor upstream release feed over AMQP"
priority = "high"
cooldown_secs = 300

[[triggers]]
type = "amqp"
routing_key = "org.fedoraproject.prod.*
condition = "$.msg.new != \"\""
```

The `SOP.md` body:

```md
## Steps

1. **Resolve** — Map the upstream project to the local package.
   - tools: shell

2. **Announce** — Post release details to the team channel.
   - tools: http_request
```
