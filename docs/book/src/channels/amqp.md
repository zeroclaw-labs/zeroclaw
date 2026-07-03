# AMQP

The `amqp` channel consumes messages from an AMQP 0-9-1 broker (RabbitMQ and compatible). Each delivery can drive the agent loop, start a SOP run, or both, selected per alias by the `dispatch` field.

> **This is a SOP event source.** For trigger syntax, routing-key matching, and the SOP side of the wiring, see [SOP Fan-In: AMQP](../sop/fan-in/amqp.md). This page covers the broker connection and the dispatch mode.

## Configuration

The full field list, derived from the live schema. For a basic consumer you set `amqp_url`, `exchange`, and `routing_keys`.

{{#config-fields channels.amqp}}

Full field reference: [config reference](../reference/config.md#channels).

## Dispatch modes

The `dispatch` field decides what a delivery does:

- `agent_loop` (default): the delivery is handed to the agent loop as a message. This preserves the original behavior; existing consumers are unaffected.
- `sop`: the delivery is lifted into a SOP event (routing key into the event topic, body into the payload) and dispatched to the SOP engine. See [SOP Fan-In: AMQP](../sop/fan-in/amqp.md).
- `sop_and_agent_loop`: both of the above run for each delivery.

## TLS

For TLS transport, point `amqp_url` at an `amqps://` endpoint and supply `ca_cert`. For mutual TLS, also set `client_cert` and `client_key`. Without these, the connection is plaintext; do not expose a plaintext consumer across an untrusted network.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| No messages consumed | exchange or routing keys do not match the publisher | Verify `exchange` and `routing_keys` against what the publisher emits |
| TLS handshake fails | `amqps://` without `ca_cert`, or a cert and key mismatch | Supply `ca_cert`; for mTLS verify `client_cert` and `client_key` pair |
| Deliveries arrive but no SOP starts | `dispatch` is `agent_loop`, or the SOP trigger does not match | Set `dispatch` to `sop` or `sop_and_agent_loop`; check the [trigger](../sop/fan-in/amqp.md) routing key |

## See also

- [SOP Fan-In: AMQP](../sop/fan-in/amqp.md): trigger syntax and routing-key matching
- [MQTT](./mqtt.md)
- [Channels overview](./overview.md)
