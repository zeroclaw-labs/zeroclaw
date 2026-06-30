# SOP Fan-In: MQTT

MQTT messages can start SOP runs. The MQTT listener subscribes to the broker, builds a SOP event per message, and dispatches it to the engine. This path is gated by the `channel-mqtt` build feature.

> The transport side (broker URL, credentials, TLS, QoS) is configured on the [MQTT channel](../../channels/mqtt.md). This page covers the trigger.

## Trigger

{{#sop-trigger mqtt}}

## Matching

Topic patterns support `+` (single level) and `#` (multi level) wildcards. The MQTT payload is forwarded into the SOP event payload, available to an optional trigger `condition`; step context receives the capped, sanitized, framed form.

## See also

- [MQTT channel](../../channels/mqtt.md): broker, TLS, QoS
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
