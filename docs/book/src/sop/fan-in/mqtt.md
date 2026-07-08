# SOP Fan-In: MQTT

MQTT messages can start SOP runs. The MQTT listener subscribes to the broker, builds a SOP event per message, and dispatches it to the engine. This path is gated by the `channel-mqtt` build feature.

> The transport side (broker URL, credentials, TLS, QoS) is configured on the [MQTT channel](../../channels/mqtt.md). This page covers the trigger.

## Trigger

{{#sop-trigger mqtt}}

## Matching

Topic patterns support `+` (single level) and `#` (multi level) wildcards. The MQTT payload is forwarded into the SOP event payload, available to an optional trigger `condition`; step context receives the capped, sanitized, framed form. A JSON-path `condition` such as `$.value > 85` requires the publisher to send a JSON body.

## Fire it

With a SOP loaded and the MQTT channel subscribed, publish a message to a topic the trigger pattern matches (for example with `mosquitto_pub`, or any broker client). The listener builds an event from the topic and payload and dispatches it. A run starts for every loaded SOP whose `topic` pattern matches and whose `condition` (if any) holds against the payload.

If nothing starts, confirm the topic matches the trigger pattern, the broker subscription is live, and the `condition` matches the payload. See the [fan-in overview troubleshooting table](./overview.md#troubleshooting).

## Approve and observe

Runs that hit a checkpoint pause as `WaitingApproval`. Clear or inspect them with the CLI (`zeroclaw sop list`, `zeroclaw sop approve`) or out-of-band over the [gateway API](../../gateway/api.md) approval endpoints (`GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`).

## See also

- [MQTT channel](../../channels/mqtt.md): broker, TLS, QoS
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
