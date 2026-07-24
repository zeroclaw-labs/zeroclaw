# SOP Fan-In: Channel

Inbound messages on an agent-loop channel (Telegram, Discord, Slack, and the other conversational channels) can start SOP runs. When a loaded SOP wants channel events, the orchestrator lifts each inbound message into a SOP event (the channel kind and optional alias become the event topic, the message content becomes the payload) and dispatches it to the engine alongside the normal agent turn.

> The transport side (bot token, allowed users, per-channel behavior) is configured on each [channel](../../channels/overview.md). This page covers the trigger. Agent-loop channels have no per-channel dispatch switch: the SOP's `channel` trigger is the opt-in, and the normal agent turn always runs alongside any SOP run it starts.

## Trigger

{{#sop-trigger channel}}

## Matching

The `channel` is a `ChannelKind` snake_case value (`telegram`, `discord`, `slack`, ...). A trigger without an `alias` matches any configured instance of that channel kind; a trigger with an `alias` matches only that instance. The event topic is `<kind>` or `<kind>/<alias>`. The message content is forwarded into the SOP event payload, available to an optional trigger `condition`; step context receives the capped, sanitized, framed form. A JSON-path `condition` such as `$.text == "deploy"` requires the message body to be JSON.

## Fire it

Load a SOP with a `channel` trigger, then send a message to that channel. The `channel` trigger is the opt-in: the orchestrator only lifts inbound messages into events when a loaded SOP wants channel events. It puts the channel kind and alias into the topic and the content into the payload, then dispatches it. A run starts for every loaded SOP whose `channel` (and `alias`, if set) matches and whose `condition` (if any) holds against the content. The normal agent turn still runs alongside any run that starts.

The fan-in hop is skipped entirely when no loaded SOP has a `channel` trigger, so channels with no channel-sourced SOP pay nothing.

All channel deliveries enter the runtime's shared SOP ingress adapter before an event is built. The
adapter owns source-interest checks, required engine/audit handle diagnostics, untrusted input
capping, event timestamps, and dispatch-result logging. Channel code supplies only the
transport-owned topic and payload. Forge events use the same boundary with their configured target
SOP, so they cannot bypass trigger matching or the normal safety and audit path.

If nothing starts, confirm a loaded SOP has a `channel` trigger, the trigger `channel`/`alias` matches the instance the message arrived on, and the `condition` matches. See the [fan-in overview troubleshooting table](./overview.md#troubleshooting).

## Approve and observe

Runs that hit a checkpoint pause as `WaitingApproval`. Clear or inspect them with the CLI (`zeroclaw sop list`, `zeroclaw sop approve`) or out-of-band over the [gateway API](../../gateway/api.md) approval endpoints (`GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`).

## See also

- [Channels: Overview](../../channels/overview.md): the transport side of each channel
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
