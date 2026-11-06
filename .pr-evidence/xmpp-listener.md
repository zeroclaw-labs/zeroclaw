> Historical compatibility-worker evidence: this records the revision named below.
> The current transport is native Rust. Current reproduction steps are in
> `crates/zeroclaw-channels/tests/xmpp_native/README.md`; the retired Python
> bridge runners and `ZEROCLAW_XMPP_PYTHON` are not current runtime setup.

# Assembled XMPP channel conversation proof

Tested public revision: `37275ff8c9177f0d482695952ad9238b1fc13031`.

Built on Linux x86_64 under WSL using the existing Rust toolchain:

```sh
cargo build --locked --release --features tools-external,channel-xmpp -j 4
```

Release build passed in 4m25s. Binary SHA-256:
`a787c8a6b2103f274bb42cf08cd258d4dc56e844ab8bab08fb18071b19482859`.
Manifest, lockfile and shared channel dispatcher matched the public commit. The XMPP Rust adapter and embedded Python worker matched after checkout CRLF/LF normalization. No production source modifications or downstream patches were included in this build.

## Boundary exercised

The real `zeroclaw --config-dir <private-fixture> channel start` process loaded the configured XMPP alias and peer group. Its production Rust listener launched its embedded Python worker, authenticated to a loopback Prosody server over certificate-verified Direct TLS, admitted the authorized bare JID, dispatched the real configured agent through the shared channel runtime and returned its answer over XMPP. Only the external model API was replaced by a deterministic local OpenAI-compatible HTTP server that counted requests.

All accounts were disposable `bot`, `alice`, and `mallory` users under `example.test`. Only alice was in the peer group. Credentials were freshly generated under a private temporary directory; no production credentials, accounts or OMEMO identity state were read. All server/client listeners used loopback. The local test CA was explicitly trusted; certificate checks were not disabled.

## Observed result

1. Mallory sent a direct chat before the authorized conversation: zero model requests and no reply.
2. Alice sent `AUTHORIZED_PROBE`: exactly one model request containing that input; Alice received `XMPP_LISTENER_PROOF_OK` from the bot through the real reply path.
3. Mallory sent another direct chat after the successful reply: the model request count stayed one and Mallory still received no reply. Neither unauthorized input appeared in model requests.
4. Both XMPP clients disconnected, the listener and Prosody fixture processes were terminated and waited, and the private temporary state was removed.

```json
{"authorized_reply":true,"model_calls":1,"unauthorized_model_calls":0,"unauthorized_replies":0,"tls_verified":true,"entrypoint":"zeroclaw channel start"}
```

This closes the listener → agent dispatch → XMPP reply gap of the earlier Python-only transport matrix and injected-event Rust tests. It is an isolated system proof with a simulated external model, not a claim of real-vendor model behavior. OMEMO was disabled in this assembled-path run; the earlier transport/OMEMO matrix remains complementary evidence and was not repeated. No production daemon was changed or restarted. Required upstream CI and merge-conflict resolution remain separate.
