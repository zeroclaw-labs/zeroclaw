# A2A Outbound Cross-Harness Verification

These fixtures support manually verifying the outbound `a2a_*` client against an
**independent** A2A implementation — the official `a2a-sdk-python` server stack
(not ZeroClaw, not a mock of our own client). This is the interop evidence the
RFC rollout gate requires, and — like any external-integration test — it is not
run by unit CI because CI has no provisioned A2A peer. Run it locally as below.

## `lifecycle_server.py` — cancel-capable independent A2A server

A small `a2a-sdk-python` server whose agent task enters a non-terminal
`WORKING` state and never completes on its own; a client can poll it via
`GetTask` and terminate it via `CancelTask` (to `CANCELED`). This exercises the
client's **nonterminal send → poll → cancel** lifecycle against an independent
implementation — the cancellation path the official `helloworld` sample cannot
provide (it raises `NotImplementedError` on `cancel`).

Requires the Python `a2a-sdk` package:

```bash
pip install a2a-sdk uvicorn
```

Start it:

```bash
python3 lifecycle_server.py        # listens on http://127.0.0.1:43100
```

## Running the client's cross-harness tests against it

With the server running, from the repo root:

```bash
# Verify the independent peer is up
curl http://127.0.0.1:43100/.well-known/agent-card.json

# Run the lifecycle (send → poll → cancel) cross-harness test that this PR uses
# as the cancellation gate:
cargo test -p zeroclaw-tools a2a_client::tests::cross_harness_send_poll_cancel_independent_peer -- --ignored

# The send → poll → terminal flow (against the official helloworld sample,
# a different independent peer, default port 9999):
#   cd a2aproject/a2a-samples/samples/python/agents/helloworld && python3 __main__.py
cargo test -p zeroclaw-tools a2a_client::tests::cross_harness_send_and_poll_official_peer -- --ignored
```

The `#[ignore]` attribute keeps these out of unit CI (no A2A peer in CI), but
they are the reproducible evidence of cross-implementation interoperability.