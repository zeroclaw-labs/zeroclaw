# XMPP / Prosody

The optional `channel-xmpp` feature adds direct, one-to-one text conversations
over XMPP. It works with Prosody and other compatible XMPP servers. Server,
port, account, transport, credentials, resource and encryption are operator
configuration, not deployment-specific constants.

## Install

```sh
cargo build --release --features channel-xmpp
```

The transport and legacy OMEMO implementation run in the Rust process. No
Python interpreter, subprocess or Python OMEMO package is required at runtime.
The optional cryptographic core is MIT licensed; its source attribution and
license are retained in `crates/zeroclaw-channels/src/xmpp_omemo/`.
The shared channel supervisor owns reconnection. The ordinary agent runtime
owns sessions, commands, tools, memory, cost tracking and replies.

## Configure in the WebUI

1. Open **Config → Channels → XMPP** and create an alias.
2. In **Connection**, set `username` to a bare account JID such as
   `bot@example.org`, the TCP `server`, `port`, `protocol`, and `password`.
   The password editor uses the normal encrypted secret store and never
   returns the saved password. Prosody with LDAP authentication uses the
   account's directory password; LDAP is a server setting, not a special
   client authentication mode.
3. In **Advanced**, choose a resource, message-size limit, connection deadline
   and OMEMO. Runtime executable selection is not a channel setting.
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
operators should install their CA in the host's trusted system certificate
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
means the TLS socket write and local reservation cleanup completed. It does
not confirm server storage, recipient decryption, delivery or human reading.
Queued sends use the same absolute deadline as the caller. Expired/cancelled
requests are refused before writing; cancellation during a write terminates
the connection and leaves the outcome uncertain.

This initial slice does not implement MUC rooms, file transfer, MAM backfill,
carbons, delivery/read receipts or cross-channel session joining. Existing
name-only announcement delivery does not establish a temporary XMPP connection;
outbound delivery uses the running channel handle.

## Optional OMEMO

`omemo_enabled = true` requires encrypted inbound and outbound text. It never
falls back to an open body when a peer lacks a usable bundle, encryption fails,
or device trust is missing. With the option off, this client does not decrypt
OMEMO or mistake encrypted fallback notices for messages to the agent.

The native implementation supports legacy OMEMO
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
trust. Every known recipient device must have an explicitly pinned identity. Removing
a pin takes effect through live canonical config policy. Device discovery
never creates trust. This initial native slice establishes new sessions from
a peer's first authenticated prekey message; proactive outbound establishment
to a peer with no session is refused. It does not implement copies to other
bot-account devices.

OMEMO identity keys and ratchet state persist in a private, per-alias
`data/channels/xmpp/<alias>/omemo.sqlite3` database (0600). Keep the containing
directory private (0700 on POSIX) and include it in confidential backups; do not regenerate it
on each restart or share it between accounts. Changing the account on an alias
with existing state fails closed. Use a new alias for a new account.

Native OMEMO state ownership is currently supported on Linux. TLS-only XMPP
uses the native transport on other supported platforms; enabling OMEMO there
fails closed. New identity creation requires an existing daemon vault key and
either a wholly absent alias directory or an exclusively owned database with
the exact TLS-only replay schema. The latter upgrades transactionally while
preserving replay entries. Existing identity, partial/corrupt or
wrong-account state, unsafe permissions, symlinks, hardlinks, WAL mode and
concurrent database owners are refused without automatic repair.

The native transport can read the compatibility worker's existing legacy state
without replacing its identity or ratchets. Stop the old process before
switching binaries, preserve the complete private alias directory and vault,
and rehearse both migration and rollback on a confidential copy first. Native
publication replenishes one-time prekeys with non-reused IDs; automatic signed
prekey rotation is not part of this initial slice.

Encrypted ingress, ratchet advancement, one-time-key consumption and replay
marking commit together. Pending input may resume before runtime admission.
Once admitted, an interrupted/unknown turn is never automatically replayed.
The encrypted ingress record links to the normal agent history; it does not
create a second tool-execution ledger. A returned turn is not proof that its
tools succeeded or its history persisted. Retention is deliberately bounded
(128 pending, 1024 active records); exhausting a bound fails closed and
requires operator reconciliation, rather than silently pruning unknown work.

Encrypted messages retain individual receipts and bypass message coalescing.
Returned `/stop`, approval-answer and event control paths are recorded separately
from agent history; a crash before recording still leaves an unknown admitted
outcome. Invalid/untrusted encrypted input is dropped without disconnecting
other peers. Broken transport or unusable local state still stops the listener.

Outgoing ciphertext is reserved atomically with all recipient ratchets. Only
a completed socket write retires that exact reservation. Failed/cancelled
writes retain the original ciphertext; no automatic re-encryption or blind
resend is performed. At 128 uncertain reservations, sending fails closed.
Always inspect the actual outcome before retrying an uncertain agent action.

The database's `public/identity` record contains the bot's public device ID and
fingerprint for verification. It contains no private key. Other records do;
never dump the whole database into logs or a prompt.

OMEMO protects the XMPP conversation up to the bot. The bot decrypts text and
passes it to its configured model provider and normal memory/logging policy.
It does not provide end-to-end encryption through a remote model provider.

## Validation

```sh
cargo test --locked -p zeroclaw-config --test xmpp
cargo test --locked -p zeroclaw-channels --no-default-features --features channel-xmpp --lib xmpp
cargo test --locked -p zeroclaw-channels --no-default-features --features channel-xmpp native_ingress_
```

The Linux-only ignored boundary test exercises the actual native listener and
shared agent dispatcher with disposable accounts. Its synthetic Python peer is
a development interoperability reference, never a runtime dependency. See
`crates/zeroclaw-channels/tests/xmpp_native/README.md` for the opt-in fixture and
commands. Both Direct TLS and STARTTLS cover existing sessions and first
contact, encrypted replies, published prekey replenishment, live policy
revocation, duplicate suppression, cancellation, encrypted ingress and
transaction failures. Unit tests also cover parser ambiguity, malformed wire
input, replay rejection, key/identity mismatch, unsafe state ownership and
expired/cancelled outbound writes.

## Offline reviewed inbox recovery

Native encrypted ingress retains at most 1,024 records, including returned work.
Reaching the limit stops admission rather than silently forgetting unknown work.
Restarting does not reclaim slots. Use the supported Linux operator command,
compiled with `channel-xmpp`, after stopping the owning daemon. Run as the same
OS account that owns the vault and state directory:

```sh
zeroclaw channel xmpp-inbox inspect VAULT_ROOT STATE_DIR ALIAS ACCOUNT PEER MESSAGE_ID
zeroclaw channel xmpp-inbox release-returned VAULT_ROOT STATE_DIR ALIAS ACCOUNT PEER MESSAGE_ID REVIEW_TOKEN EVIDENCE_REFERENCE
zeroclaw channel xmpp-inbox inspect-release VAULT_ROOT STATE_DIR ALIAS ACCOUNT PEER MESSAGE_ID
```

Use canonical absolute directory paths. `VAULT_ROOT` is the existing directory
containing `.secret_key`; `STATE_DIR` contains the alias's `omemo.sqlite3`.
Inspection returns authenticated metadata and a review token, never message
content or private keys. Review the linked runtime history or terminal control
outcome before passing the exact token and a bounded operator evidence reference
to `release-returned`. The command requires exclusive ownership, refuses pending
or uncertain admitted work and stale tokens, and atomically preserves an
encrypted identity-bound release receipt and the canonical replay marker while
removing the returned payload. Resume the daemon after the review. No agent tool,
background maintenance job, automatic bulk deletion or replay is registered.

Known pre-turn outcomes (unowned route, budget/lane refusal, retired agent,
hook cancellation, and cancellation while queued) are recorded as terminal
control outcomes without inventing a runtime-history link. An aborted/crashed
execution remains admitted and unknown. Reviewed release must never be used to
claim that returned processing proves successful tools or external delivery.
