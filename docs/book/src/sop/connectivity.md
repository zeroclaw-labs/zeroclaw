# SOP Connectivity & Event Fan-In

This document describes how external events trigger SOP runs.

## Quick Paths

- [MQTT Integration](#2-mqtt-integration)
- [Other Trigger Types](#3-other-trigger-types)
- [Security Defaults](#4-security-defaults)
- [Troubleshooting](#5-troubleshooting)

## 1. Overview

SOP runs are driven by events delivered to the SOP engine through `dispatch_sop_event`. The engine matches each event against every loaded SOP's triggers and starts runs for those that match.

Key behaviors:

- **Consistent trigger matching:** one matcher path evaluates all trigger types.
- **Run-start audit:** started runs are persisted via `SopAuditLogger`.
- **Headless safety:** in non-agent-loop contexts, `process_headless_results` logs `ExecuteStep` actions as pending instead of silently executing them.

Of the defined trigger types, **MQTT** is the only event source wired to a live listener in the daemon. Runs can also be started directly from an agent turn with the `sop_execute` tool. The webhook, cron, and peripheral trigger types are defined and matched by the engine, but no runtime currently feeds those event sources into `dispatch_sop_event` — see [Other Trigger Types](#3-other-trigger-types).

## 2. MQTT Integration

MQTT is delivered by `run_mqtt_sop_listener`, which subscribes to the broker, builds an MQTT `SopEvent` per message, and calls `dispatch_sop_event`. This path is gated by the `channel-mqtt` build feature.

### 2.1 Configuration

Configure broker access with `zeroclaw config set channels.mqtt.<field> <value>`: the keys land under `[channels.mqtt]` in the stored config. See the [Config reference](../reference/config.md) for all fields. The `use_tls` flag must match the scheme of `broker_url` (`mqtts://` ⇒ `true`, `mqtt://` ⇒ `false`).

### 2.2 Trigger Definition

The SOP's trigger is defined in its `SOP.toml` (see [Syntax](./syntax.md)). Topic patterns support `+` (single-level) and `#` (multi-level) wildcards. The MQTT payload is forwarded into the SOP event payload (`event.payload`) and is available to an optional trigger `condition`, then shown in step context.

## 3. Other Trigger Types

The engine defines and matches three further trigger types, but no live event source currently routes events into the dispatcher for them. The trigger syntax is accepted in `SOP.toml` and validated (see [Syntax](./syntax.md)); the matching logic is exercised by tests, but the runtime fan-in is not yet wired.

| Trigger type | Matcher | Wiring status |
|---|---|---|
| `webhook` | Exact match against the trigger `path` | No HTTP route delivers webhook `SopEvent`s; the gateway has no SOP endpoint. |
| `cron` | `check_sop_cron_triggers` performs a window-based check over cached cron triggers | Defined, but no scheduler caller invokes it outside tests. |
| `peripheral` | Exact match against `"{board}/{signal}"` | `dispatch_peripheral_signal` exists, but no peripheral listener calls it outside tests. |

Defining one of these triggers in a `SOP.toml` is valid and will not error, but the SOP will only ever start via MQTT or `sop_execute` until the corresponding event source is wired.

## 4. Security Defaults

| Feature | Mechanism |
|---|---|
| **MQTT transport** | `mqtts://` + `use_tls = true` for TLS transport |
| **Cron validation** | Invalid cron expressions fail closed during parsing/cache build |
| **Headless dispatch** | Headless callers log run progression instead of auto-executing `ExecuteStep` |

## 5. Troubleshooting

| Symptom | Likely Cause | Fix |
|---|---|---|
| **MQTT** connection errors | broker URL/TLS mismatch | Verify scheme + TLS flag pairing (`mqtt://`/`false`, `mqtts://`/`true`) |
| **MQTT** SOP not starting | topic pattern mismatch or failing `condition` | Verify the trigger topic/wildcards match the published topic; check the `condition` against the payload |
| **SOP started but step not executed** | headless trigger without active agent loop | run an agent loop for `ExecuteStep`, or design the run to pause on approvals |
| **Webhook/cron/peripheral trigger never fires** | event source not wired into the dispatcher | use an MQTT trigger or start the run with `sop_execute` |
