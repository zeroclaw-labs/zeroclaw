# MQTT

The `mqtt` channel subscribes to topics on an MQTT broker and feeds each message into the agent loop or the SOP engine. It is gated by the `channel-mqtt` build feature.

> **This is a SOP event source.** For trigger syntax and topic matching, see [SOP Fan-In: MQTT](../sop/fan-in/mqtt.md). This page covers the broker connection.

## Configuration

The full field list, derived from the live schema. For a basic subscriber you set `broker_url` and `topics`.

{{#config-fields channels.mqtt}}

Full field reference: [config reference](../reference/config.md#channels).

## TLS

Set `use_tls` to match the scheme of `broker_url`: `mqtts://` pairs with `use_tls = true`, `mqtt://` with `use_tls = false`. A mismatch is the most common connection failure.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Connection errors at startup | broker URL and TLS flag disagree | Pair the scheme with `use_tls` (`mqtt://` with `false`, `mqtts://` with `true`) |
| Subscribed but no messages | topic filter does not match the published topic | Verify `topics` and wildcards against what the publisher emits |
| SOP not starting | topic mismatch or a failing `condition` | Check the [trigger](../sop/fan-in/mqtt.md) topic and condition against the payload |

## See also

- [SOP Fan-In: MQTT](../sop/fan-in/mqtt.md): trigger syntax and topic matching
- [AMQP](./amqp.md)
- [Channels overview](./overview.md)
