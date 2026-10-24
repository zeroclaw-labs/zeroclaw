# XMPP / Prosody

The optional `channel-xmpp` feature adds direct, one-to-one text conversations
over XMPP. It works with Prosody and other compatible XMPP servers. Server,
port, account, transport, credentials, resource and encryption are operator
configuration, not deployment-specific constants.

## Install

```sh
cargo build --release --features channel-xmpp
python3 -m venv /opt/zeroclaw-xmpp
/opt/zeroclaw-xmpp/bin/pip install -r crates/zeroclaw-channels/xmpp-requirements.txt
```

For the current compatibility worker, set `ZEROCLAW_XMPP_PYTHON` in the trusted
service environment to `/opt/zeroclaw-xmpp/bin/python` before starting the daemon.
Its default is `python3`. Restart after changing that environment. The WebUI,
config API and channel TOML cannot choose an executable. Legacy `python_path`
values are ignored; migrate that setting to the service environment before
upgrading an existing virtualenv deployment. Do not put passwords in this variable.

The Rust channel uses an embedded, isolated Python transport worker with
Slixmpp. The worker performs XMPP and optional OMEMO; it does not call the
gateway or run a second agent loop. The existing channel supervisor owns
reconnection, and the shared runtime owns sessions, commands, tools, memory,
cost tracking and replies. Missing dependencies fail closed.

Slixmpp is MIT licensed. The optional `slixmpp-omemo` backend is
**AGPL-3.0-only** and is not vendored or automatically installed by Cargo.
Review its license when choosing to install and redistribute that backend.
For TLS-only deployments install only `slixmpp==1.12.0`.

## Configure in the WebUI

1. Open **Config → Channels → XMPP** and create an alias.
2. In **Connection**, set `username` to a bare account JID such as
   `bot@example.org`, the TCP `server`, `port`, `protocol`, and `password`.
   The password editor uses the normal encrypted secret store and never
   returns the saved password. Prosody with LDAP authentication uses the
   account's directory password; LDAP is a server setting, not a special
   client authentication mode.
3. In **Advanced**, choose a resource, message-size limit, connection deadline
   and OMEMO. The interpreter is an operator-controlled process setting.
4. In **Allowed users**, create an authorization policy for this channel or
   edit an existing matching policy. The create button makes a canonical peer
   group scoped to `xmpp.<alias>`. Put bare JIDs in `external_peers`; `ignore`
   entries override grants, including wildcards.
   Empty authorization denies everyone; there is no open registration or
   automatic contact approval. This reuses canonical peer-group policy rather
   than storing a second, conflicting transport allowlist.
5. Bind `xmpp.<alias>` to an enabled agent under **Agents → Channels**. Save
   the settings, enable the channel with the toggle in its header, and reload
   the daemon.

The default is **disabled**, Direct TLS, port **5223**, and OMEMO **off**.
STARTTLS deployments normally use **5222**. A server may offer XMPP on **443**,
but that is XMPP over TLS, not HTTPS. Port and transport are independent
settings; switching transport does not silently rewrite the port.

Both modes require TLS and certificate verification against the account JID's
domain. `server` only overrides the TCP destination. There is no unencrypted
fallback, trust-all switch or certificate-verification bypass. Private-CA
operators should install their CA in the worker's trusted system certificate
store, not disable verification.

```toml
[channels.xmpp.private]
enabled = false # Finish credentials, authorization and binding before enabling.
username = "bot@example.org"
server = "xmpp.example.org"
port = 5222
protocol = "starttls" # Or direct_tls with the server's Direct TLS port.
resource = "zeroclaw"
# Configure password via the secret-aware WebUI or CLI, not a shared file.
omemo_enabled = false

[peer_groups.private_chat]
channel = "xmpp.private"
agents = ["assistant"]
external_peers = ["alice@example.org"]
```

## Sessions and message handling

A message from an authorized account enters the ordinary channel runtime and
creates or continues that peer's conversation. The alias and normalized bare
sender JID scope the session; switching phone/desktop resources does not create
different identities. Different aliases and users do not share chat history
implicitly. Runtime commands such as `/new` and `/stop` use the shared path.

Only direct `chat` and `normal` messages with bodies are admitted. Self echoes,
groupchat, forwarded envelopes, sent/received carbons and delayed archive
messages are ignored. A bounded SQLite message-ID cache suppresses replay over
reconnects. Oversized inbound bodies and unsupported attachments are rejected.
Outbound sends are also subject to the live peer policy. A successful send
means the stanza was queued to the TLS stream, not that a human read it.

This initial slice does not implement MUC rooms, file transfer, MAM backfill,
carbons, delivery/read receipts or cross-channel session joining. Existing
name-only announcement delivery does not establish a temporary XMPP connection;
outbound delivery uses the running channel handle.

## Optional OMEMO

`omemo_enabled = true` requires encrypted inbound and outbound text. It never
falls back to an open body when a peer lacks a usable bundle, encryption fails,
or device trust is missing. With the option off, this client does not decrypt
OMEMO or mistake encrypted fallback notices for messages to the agent.

The pinned stable backend supports legacy OMEMO
`eu.siacs.conversations.axolotl`, used by compatible Conversations/Gajim
deployments. It does **not** claim `urn:xmpp:omemo:2` support. The server needs
working PEP/pubsub device-list and bundle storage; encryption and decryption
remain client operations. Merely enabling a server module is not sufficient.

Set `omemo_trusted_devices` in **Advanced** to entries of the form:

```text
alice@example.org <64-hex-character-Curve25519-fingerprint>
```

Grouped fingerprints with spaces are accepted; the legacy `05` key-type prefix
is accepted too. Verify fingerprints out of band. There is no blind first-use
trust. Include the peer devices and any other devices of the bot's own account
that should receive copies. Removing a fingerprint and reloading revokes its
persisted trust. New or replaced devices require a new explicit trust decision.

OMEMO identity keys and ratchet state persist in a private, per-alias
`data/channels/xmpp/<alias>/omemo.sqlite3` database (0600). Keep the containing
directory private (0700 on POSIX) and include it in confidential backups; do not regenerate it
on each restart or share it between accounts. Changing the account on an alias
with existing state fails closed. Use a new alias for a new account.

The transport worker is validated on Linux with a POSIX asyncio pipe loop.
Native Windows worker execution is not currently covered by these tests.

The database's `public/identity` record contains the bot's public device ID and
fingerprint for verification. It contains no private key. Other records do;
never dump the whole database into logs or a prompt.

OMEMO protects the XMPP conversation up to the bot. The bot decrypts text and
passes it to its configured model provider and normal memory/logging policy.
It does not provide end-to-end encryption through a remote model provider.

## Validation

```sh
cargo test -p zeroclaw-config --test xmpp
cargo test -p zeroclaw-channels --features channel-xmpp xmpp --lib
/opt/zeroclaw-xmpp/bin/python crates/zeroclaw-channels/tests/test_xmpp_bridge.py
```

`tests/xmpp_bridge_integration.py` is an opt-in real-server test using disposable
`bot`, `alice` and `mallory` accounts on `example.test`, a loopback Prosody with
STARTTLS on 15222 and Direct TLS on 15223, PEP/pubsub, and a temporary trusted
test certificate for that domain. Pass `--fixture` with a private JSON mapping
those three account names to passwords, `--certificate` with that CA file,
and `--python` with the venv interpreter. It checks both transports, invalid
certificates, actual encrypted round trips, identity persistence, plaintext
rejection and revocation without downgrade. Never use production accounts.
