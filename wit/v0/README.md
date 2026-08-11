# wit/v0 — zeroclaw:plugin@0.x

Status: **Experimental** — This version is currently experimental and can be freely modified until the Component Model ABI ships; no `wit/v0/.frozen` marker is present.

The base worlds are gated behind the `plugins-wit-v0` feature (see `@unstable`
annotations). Optional host surfaces use additional WIT feature gates: tool and
channel components opt into host-mediated TCP/TLS/STARTTLS with
`plugins-wit-v0-sockets` and WebSockets with `plugins-wit-v0-websocket`.

**Stability fence**: `wit/v0/.frozen` does not yet exist. It is created in a
dedicated PR when `zeroclaw-plugins` ships the first 0.1.0 stable release with
the Component Model path enabled. After that point only additive changes via
`@since(version = 0.x.0)` are accepted in this directory. See `wit/VERSIONING.md`
for how to create a stabilizing PR.
