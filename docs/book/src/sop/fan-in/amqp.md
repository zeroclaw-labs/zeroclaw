# SOP Fan-In: AMQP

AMQP deliveries can start SOP runs. When an alias runs in a SOP dispatch mode, the AMQP consumer lifts each delivery into a SOP event (the routing key becomes the event topic, the message body becomes the payload) and dispatches it to the engine.

> The transport side (broker connection, queue, exchange, TLS) is configured on the [AMQP channel](../../channels/amqp.md). This page covers the trigger. The dispatch mode that decides whether deliveries drive the agent loop, the SOP engine, or both is the channel's `dispatch` field.

## Trigger

{{#sop-trigger amqp}}

## Matching

The `routing_key` uses AMQP topic-exchange semantics: keys are `.`-delimited words, `*` matches exactly one word, and `#` matches zero or more words. The delivery body is forwarded into the SOP event payload, available to an optional trigger `condition`; step context receives the capped, sanitized, framed form. A JSON-path `condition` such as `$.value > 85` requires the publisher to send a JSON body.

## Fire it

Set the channel's `dispatch` field to a SOP mode (`sop` or `sop_and_agent_loop`), load a SOP, then publish a message to the exchange with a routing key the trigger matches. The consumer lifts the delivery into an event (routing key into topic, body into payload) and dispatches it. A run starts for every loaded SOP whose `routing_key` pattern matches and whose `condition` (if any) holds against the body.

If nothing starts, confirm `dispatch` is a SOP mode, the queue is bound so the routing key actually reaches the consumer, and the `condition` matches. See the [fan-in overview troubleshooting table](./overview.md#troubleshooting).

## Approve and observe

See [Approve and observe](./overview.md#approve-and-observe) for pending runs, approval commands, and run logs.

## See also

- [AMQP channel](../../channels/amqp.md): broker, queue, exchange, TLS
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
