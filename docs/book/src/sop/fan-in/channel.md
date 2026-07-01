# SOP Fan-In: Channel

Inbound messages on an agent-loop channel (Telegram, Discord, Slack, and the other conversational channels) can start SOP runs. When a channel's SOP dispatch mode is active, the orchestrator lifts each inbound message into a SOP event (the channel kind and optional alias become the event topic, the message content becomes the payload) and dispatches it to the engine alongside the normal agent turn.

> The transport side (bot token, allowed users, per-channel behavior) is configured on each [channel](../../channels/overview.md). This page covers the trigger. Whether an inbound message drives the agent loop, the SOP engine, or both is the channel's `dispatch` field.

## Trigger

{{#sop-trigger channel}}

## Matching

The `channel` is a `ChannelKind` snake_case value (`telegram`, `discord`, `slack`, ...). A trigger without an `alias` matches any configured instance of that channel kind; a trigger with an `alias` matches only that instance. The event topic is `<kind>` or `<kind>/<alias>`. The message content is forwarded into the SOP event payload, available to an optional trigger `condition`; step context receives the capped, sanitized, framed form. A JSON-path `condition` such as `$.text == "deploy"` requires the message body to be JSON.

## Fire it

Set the channel's `dispatch` field to a SOP mode (`sop` or `sop_and_agent_loop`), load a SOP with a `channel` trigger, then send a message to that channel. The orchestrator lifts the inbound message into an event (channel kind and alias into topic, content into payload) and dispatches it. A run starts for every loaded SOP whose `channel` (and `alias`, if set) matches and whose `condition` (if any) holds against the content.

The fan-in hop is skipped entirely when no loaded SOP has a `channel` trigger, so channels with no channel-sourced SOP pay nothing.

If nothing starts, confirm `dispatch` is a SOP mode, the trigger `channel`/`alias` matches the instance the message arrived on, and the `condition` matches. See the [fan-in overview troubleshooting table](./overview.md#troubleshooting).

## Approve and observe

Runs that hit a checkpoint pause as `WaitingApproval`. Clear or inspect them with the CLI (`zeroclaw sop list`, `zeroclaw sop approve`) or out-of-band over the [gateway API](../../gateway/api.md) approval endpoints (`GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`).

## See also

- [Channels: Overview](../../channels/overview.md): the transport side of each channel
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
