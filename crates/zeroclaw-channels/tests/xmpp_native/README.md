# Native XMPP interoperability fixture

This opt-in Linux test uses the actual Rust channel, shared agent dispatcher,
and a legacy OMEMO reference peer. Python packages here are development-only;
the native channel does not spawn or import them. The optional reference
OMEMO package has its own AGPL license; it is not vendored into the Rust binary.

## Isolated server

For a self-contained run on Linux with Prosody and OpenSSL installed, use the
managed fixture after installing the development requirements below:

```sh
python crates/zeroclaw-channels/tests/xmpp_native/managed_server.py --output /absolute/untracked/native-xmpp-result.json
```

This creates disposable accounts and a one-day test certificate, binds only to
loopback, runs the four native listener scenarios, then stops its own server
and removes its private state. It refuses occupied fixture ports. The XMPP CI
lane uses this same entrypoint. No production XMPP account or server is needed.

Use a disposable local Prosody instance, never a production server. Provide:

- Domain `example.test`; synthetic `bot`, `bob`, `alice`, `mallory` accounts.
- Mandatory verified TLS: STARTTLS on `127.0.0.1:15222` and Direct TLS on
  `127.0.0.1:15223`. A test certificate must cover `example.test`.
- PEP/pubsub with device-list/bundle storage and publish-options support.
- A private fixture directory (0700) with `certs/cert.pem` (test CA) and
  `fixture-accounts.json` (0600), mapping those four account names to their
  disposable passwords. Keep certificate private keys and server data private.

These ports and identities are fixed synthetic fixtures. The driver connects
only to loopback. Other OMEMO devices are preserved when publishing the bot
list. Fixtures and results must stay outside tracked source files.

## Run

From the repository root, create a development virtual environment, install
`crates/zeroclaw-channels/tests/xmpp_native/requirements.txt`, and run:

```sh
cargo test --locked -p zeroclaw-channels --no-default-features --features channel-xmpp --lib xmpp
cargo test --locked -p zeroclaw-channels --no-default-features --features channel-xmpp native_ingress_
python crates/zeroclaw-channels/tests/xmpp_native/integration.py --server-root /absolute/private/fixture --output /absolute/untracked/native-xmpp-result.json
```

Set `CARGO_TARGET_DIR` to an existing build directory if needed. Reserve enough
space on the physical volume hosting that directory, including any WSL VHD;
free space reported inside WSL alone does not establish host capacity. The
driver builds the executable before starting protocol readiness deadlines.

The four scenarios cover both transports and both existing and fresh sessions.
Assertions reach a real encrypted reply from the shared dispatcher, then check
live pin/allowlist revocation, duplicate suppression, identity preservation,
published one-time prekeys and their non-reused IDs, native identity creation,
exclusive state ownership, cancellation/reopen, encrypted ingress and canonical
history linkage. Child-process probes exercise crash admission and offline
reviewed release through the same production administrative implementation exposed by
`zeroclaw channel xmpp-inbox`. The operator regression fills the 1,024-record
limit, releases one reviewed returned item, admits a new item, and rejects replay.
An injected reservation-cleanup fault must preserve the exact ciphertext.

Expired/cancelled send unit tests cover the socket boundary separately. A
socket write acknowledgement is not a delivery/read receipt. No fixture asserts
that returned agent processing proves successful tool effects or persisted
history. No production identity, credential or database is used or exported.
