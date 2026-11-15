# Response parsing boundary

`zeroclaw-tool-call-parser` owns pure protocol detection and incremental text
state: reasoning tags, terminal markers and text-encoded tool calls.

The runtime converts admitted tool specifications into parser input, consumes
diagnostics, delivers events and owns cancellation. It retains tool policy,
authorization and execution. Parsing does not grant permission to execute.

This extraction preserves existing configuration and leaves per-model policy
configuration to the provider registry change.
