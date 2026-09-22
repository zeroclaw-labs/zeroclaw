# gemini-live

A focused Rust client for the Google Gemini Live API.

It owns everything about talking to Gemini Live correctly — wire types, setup
serialization, server-message parsing (including affective emotion tokens), the
WebSocket transport (HTTP CONNECT proxy + TLS + close diagnostics), and a
reconnect/resumption session driver — behind a small async event API. Callers
handle their own application semantics on top.

Extracted from the [kutsu](https://github.com/metalmon/kutsu) outbound-calling
server, where it is consumed as a git submodule.

## Status

Early. Built layer-by-layer: `types` → `wire` → `transport` → `session`.

## License

MIT — see [LICENSE](LICENSE).
