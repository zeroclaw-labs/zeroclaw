# SOP Fan-In: AMQP

AMQP deliveries can start SOP runs. When an alias runs in a SOP dispatch mode, the AMQP consumer lifts each delivery into a SOP event (the routing key becomes the event topic, the message body becomes the payload) and dispatches it to the engine.

> The transport side (broker connection, queue, exchange, TLS) is configured on the [AMQP channel](../../channels/amqp.md). This page covers the trigger. The dispatch mode that decides whether deliveries drive the agent loop, the SOP engine, or both is the channel's `dispatch` field.

## Trigger

{{#sop-trigger amqp}}

## Matching

The `routing_key` uses AMQP topic-exchange semantics: keys are `.`-delimited words, `*` matches exactly one word, and `#` matches zero or more words. The delivery body is forwarded into the SOP event payload, available to an optional trigger `condition`; step context receives the capped, sanitized, framed form.

## See also

- [AMQP channel](../../channels/amqp.md): broker, queue, exchange, TLS
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
