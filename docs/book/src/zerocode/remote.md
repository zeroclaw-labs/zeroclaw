# Remote setup (WSS)

Connect zerocode on your workstation to a daemon running on another machine
(Raspberry Pi, home server, VPS, etc.).

For the full knob-by-knob guide covering all three topologies (direct,
daemon-to-relay, and client-through-relay), see
[Secure transport (end-to-end config)](./secure-transport.md).

> **The WSS plane is mutually authenticated (mTLS).** Every client presents a
> certificate; there is no server-only / unauthenticated path. The easy way to
> get a client certificate is **enrollment** (below) - you do not hand-manage
> certs. If you would rather mint one yourself, see the bring-your-own steps
> in "On your workstation" further down. `--tls-skip-verify` only relaxes
> *server* certificate verification for a self-signed daemon cert; the client
> certificate is still required either way.

**You need two things: a certificate and a bearer token.** The client
certificate secures the transport: it proves which device is on the other end
of the TLS connection. It does not say who you are. Every remote connection
must also present a **bearer token** in the `initialize` handshake, and that
token is what identifies you (see
[Bearer token](#bearer-token-required-for-every-connection) below). Enrollment
gives you only the certificate. A client with a certificate but no token
completes the TLS handshake and is then refused with `AUTH_REQUIRED`
(`-32010`).

## Sign in with OIDC (recommended for OIDC deployments)

An administrator must first configure an `[oidc.<alias>]` relationship and
permission mappings on the daemon, enable its device authorization grant at
the identity provider, enable WSS, and expose the gateway over HTTPS. A human
permission profile must grant `sessions:create` with at least one allowed
agent for self-service device bootstrap; `system:execute` or admin also allows
it. This is an
issuer/group policy, not registration of an individual user or device. No local
`[users]` entry, paired token, pending code, existing client certificate, or
separate `[enroll]` listener is needed. The alias below is that daemon-side
alias, not an identity-provider brand or issuer URL. Run the gateway as part
of `zeroclaw daemon`, which owns the certificate CA and shared audit authority;
a separately started gateway does not issue device certificates.

```sh
zerocode login corp wss://daemon.example.com:9781 \
  --gateway-url https://gateway.example.com
zerocode
```

For the standard ports, `zerocode login corp daemon.example.com` uses WSS
port 9781 and the gateway at
`https://daemon.example.com:42617`. The gateway itself does not supply HTTPS
by default: use `--gateway-url` to name its TLS-terminating proxy. A gateway
path prefix is supported. Plain HTTP is accepted only on loopback.

Public clients that enforce PKCE are supported: zerocode sends an S256
challenge on device authorization and the matching verifier only when polling
for its token. The verifier lives only for the sign-in, is never persisted, and
is not part of the gateway's device-start response.

Login works from a completely empty client configuration directory:

1. Verify the gateway's HTTPS certificate. A public certificate needs no prompt;
   use `--gateway-ca-cert <pem>` for a private gateway CA. An untrusted
   certificate offers an explicit skip-verification confirmation; declining or
   EOF aborts before OIDC credentials are exchanged.
2. Show the identity provider's verification URL and code. Approve the sign-in
   in your browser. This is the IdP's device-authorization code, **not a daemon
   pairing or enrollment code**. No client secret is needed on your workstation.
3. Generate a new key and CSR locally. The daemon verifies the OIDC access token
   and current permission mapping, creates the previously unknown device, and
   signs its CSR through `POST /api/oidc/<alias>/certificate` on that same
   authenticated gateway connection. The private key never leaves the client.
   The gateway returns the daemon CA, so no SAS or out-of-band code is needed.
   The separate port-9782 enrollment service is never contacted.
4. Verify WSS using the returned CA (or `--tls-ca-cert <pem>` if explicitly
   configured) and initialize with the new certificate and OIDC access token.
   WSS stays mutually authenticated. Its TLS trust remains separate from the
   gateway's: a deliberate skip on one peer never implicitly skips the other.
5. Save the working login and select that URL as the default only after the
   daemon accepts the RPC handshake. Failure does not replace a saved login.

`--tls-skip-verify` still requires confirmation for each TLS peer. Choosing
**always** remembers only that exact URL, not every URL on the host. Prefer
verified HTTPS: a certificate obtained through an explicitly unverified gateway
inherits the trust risk you accepted for that gateway.

Later, plain `zerocode` uses the selected login. To select a different saved
login for one run, use `zerocode --connect wss://other.example.com:9781`.
Each normalized **WSS URL**, including its port and path, has its own login;
logging into a second URL does not overwrite the first URL's credentials.
A complete `wss://` URL without an explicit port means standard WSS port 443;
a bare hostname without a port means ZeroClaw's WSS port 9781.

Tokens and expiry metadata live in
`[connection.logins."wss://daemon.example.com:9781/"]` in the local
`zerocode-config.toml`. They are stored **in plaintext**, never printed by
login, and written atomically with owner-only permissions (`0600`, directory
`0700` on Unix; protect the directory ACL on other platforms). Saves of that
file, including startup renewal, are serialized, so two logins that share a
config directory cannot erase each other's URL. Treat this file as a
credential: do not commit or share it. Device certificates and private
keys are isolated under
`<config-dir>/connections/<url-sha256>/generations/<generation>/tls/` and use
the existing crash-recoverable credential writer. The saved certificate path
selects the active generation; older URL-scoped `tls/` caches remain readable.
A replacement generation is validated with WSS before the config switches to
it. A refused login removes only its candidate files, never the previous key
or trust anchor. Previous accepted generations are retained, and a config-save
error can leave an unselected candidate rather than risk deleting a referenced
key; protect these directories as credentials too.

Before starting the TUI, zerocode asks you to sign in again if the saved token
is expired, the device certificate is missing, malformed, expired or not yet
valid, or the daemon rejects its authentication. Certificate lifetime is read
from the actual X.509 certificate, independently of token expiry or cached
profile metadata. It saves the replacement automatically. There is no refresh-token cache or silent background refresh;
if a token expires during an active TUI session, restart zerocode to sign in
again. You can also rerun `zerocode login corp <url>` explicitly. A saved
custom gateway URL is reused when omitted on a subsequent login.

Login issues a device client certificate valid for 30 days. `cert/renew` is
authenticated by the presented client certificate, not by an OIDC principal,
so 30 days is not an absolute device lifetime: zerocode renews about halfway
through, and an unrevoked device can keep renewing. Removing a user's OIDC
mapping stops principal-authorized operations, but it does not retire the
device or stop renewal.

Revoking the device is a separate operator step:

1. Run `zeroclaw security list-client-certs`. The certificate issued at
   sign-in has an `actor` that starts with `oidc:` and names the user's
   issuer and subject. Note its `device`.
2. Run `zeroclaw security revoke-client-cert --device <id>`. That revokes
   every active certificate the device holds.

Revoke by device, not by fingerprint. A renewed certificate is recorded with
the actor `renew`, so only the device ID ties it to the user, and revoking
one fingerprint leaves the device's other certificates able to renew. Each
sign-in that issues a certificate creates a new device ID, so one user can
have several. The CRL is checked at the next TLS handshake, so a revoke does
not drop an already established connection.

A leaked access token can be used to obtain a device certificate, and that
device can keep renewing until it is revoked. The certificate alone
authorizes only renewal: every other operation still needs a valid access
token.

Startup renewal and a rejected-token retry honor the current `--tls-ca-cert`,
`--tls-client-cert`, `--tls-client-key`, and `--tls-skip-verify` choices. Those
one-run flags are not copied into the saved login during automatic renewal;
use an explicit `zerocode login` invocation to save replacement TLS settings.

`ZEROCLAW_AUTH_TOKEN` still overrides the selected token for a one-off run;
login warns if that environment variable would shadow the token it just
saved. Existing manually configured `[connection.wss]` credentials continue to
work for connections without a saved URL login.

Use `--config-dir` to select a separate client configuration directory.
`--enroll` forces a fresh OIDC-authorized device certificate, without any
pairing prompt. `--enroll-host` and `--enroll-port` belong to the separate
pairing-based flow, not OIDC login. If a certificate was provisioned out of band,
pass `--tls-client-cert` and `--tls-client-key` instead. Login currently targets
a direct WSS URL; it does not log in through a relay-served dashboard.

## Pairing-based enrollment (non-OIDC clients)

The first time you connect a certless client interactively, zerocode enrolls
automatically:

```sh
zerocode --connect wss://<remote-host>:9781
```

It prompts for the daemon's one-time **pairing code** (printed in the daemon's
log on start), shows a **short-auth-string (SAS)** to confirm against the daemon
console (so a man-in-the-middle CA is caught), then fetches and caches a client
certificate under `<config-dir>/tls`. Later runs reuse that certificate
without prompting again, and it auto-renews at ~50% of its lifetime. To enroll
non-interactively use `zerocode --enroll --connect wss://<remote-host>:9781`.

Enrollment does not give you a bearer token. The enrollment pairing code is
separate from the gateway pairing code, and it is used up by enrollment. You
still need a token before the daemon accepts a session; see
[Bearer token](#bearer-token-required-for-every-connection).

A certless client that reaches the WSS plane without enrolling gets an actionable
"enroll first" message (and the daemon logs the rejected un-migrated client) -
never a silent hang. A **revoked** certificate is refused at the handshake
(driven by the issued-cert ledger), so revoking a lost device takes effect on
its next connection.

`allow_unpaired_enrollment` in the `[enroll]` section is reserved for a future
code-less migration flow. This release rejects any non-empty value at daemon
startup; leave it empty and use the printed pairing code.

## On the remote host (daemon side)

1. **Enable WSS.** Set the `wss` config through the [Config](./config.md) pane (or the gateway / `zeroclaw config set`):

   ```toml
   [wss]
   enabled = true
   ```

   Leave `cert_path` / `key_path` empty (the default): the daemon auto-generates
   its own CA and server certificate under `<data_dir>/tls/` on first boot, so
   you do not need to run `openssl` yourself. Set them only to bring your own
   server certificate, and use absolute paths either way; the config does not
   expand `~`.

2. **Open the firewall port:**

   <div class="os-tabs-src">

   #### sh

   ```sh
   sudo ufw allow 9781/tcp
   ```

   </div>

   The default WSS port is **9781**. Change it with `port = <number>` in the `[wss]` section.

3. **Start (or restart) the daemon:**

   <div class="os-tabs-src">

   #### sh

   ```sh
   zeroclaw daemon
   ```

   </div>

   You should see a log line confirming the WSS listener started on `0.0.0.0:9781`.

## On your workstation (zerocode side)

Enrollment (above) is the fastest way to get a client certificate. If you would
rather bring your own client certificate instead of enrolling interactively:

1. **Issue a client certificate on the daemon host**, from the daemon's mTLS CA:

   ```sh
   zeroclaw security issue-client-cert --name my-laptop --out-dir /tmp/my-laptop-tls
   ```

   This writes `ca.crt`, `client.crt`, and `client.key` to `--out-dir`. Add
   `--force` to overwrite an existing certificate for that name.

2. **Copy the three files to the workstation's `<config-dir>/tls/`** (then
   `zerocode --connect wss://<remote-ip>:9781` finds them automatically), or
   point at them explicitly:

   <div class="os-tabs-src">

   #### sh

   ```sh
   zerocode --connect wss://<remote-ip>:9781 \
     --tls-ca-cert     /path/ca.crt \
     --tls-client-cert /path/client.crt \
     --tls-client-key  /path/client.key
   ```

   </div>

   Because `ca.crt` is the same CA that signed the daemon's server certificate,
   this verifies the server too; `--tls-skip-verify` is only needed if you skip
   `--tls-ca-cert` against a self-signed daemon cert you have not pinned.

Either way, the certificate only gets you through TLS. Add a bearer token next.

## Bearer token (required for every connection)

The daemon refuses any remote `initialize` that has no `auth_token`, even
when the client certificate is valid. Give zerocode a token once, and it
presents the token on every connection and reconnect.

1. **Pair with the gateway to get a token.** Use the dashboard's Pairing page,
   or, on the daemon host (the gateway listens on `127.0.0.1:42617` by
   default), POST the gateway pairing code from the daemon log to `/pair`:

   ```sh
   curl -X POST http://127.0.0.1:42617/pair -H 'X-Pairing-Code: <code>'
   ```

   The response carries a `zc_...` token. Use the gateway's pairing code here,
   not the enrollment code you gave zerocode above.

   A daemon built without the gateway (the CLI-only build from
   `install.sh --cli-only`) has no `/pair` and no Pairing page, so it cannot
   issue this token. Connect with an OIDC token instead (below), or run
   zerocode on the server itself (`ssh -t <remote-host> zerocode`): a zerocode
   on the daemon host talks to the local socket and needs no bearer token.

2. **Give zerocode the token.** The environment variable is the recommended
   way because it keeps the secret out of the config file:

   ```sh
   export ZEROCLAW_AUTH_TOKEN=zc_...
   zerocode --connect wss://<remote-host>:9781
   ```

   Or set it in zerocode's config, either as a path to a file that only you
   can read or inline:

   ```toml
   [connection.wss]
   uri = "wss://<remote-host>:9781"
   auth_token_file = "/abs/path/zerocode-bearer"   # or: auth_token = "zc_..."
   ```

   Precedence is `ZEROCLAW_AUTH_TOKEN`, then `auth_token_file`, then
   `auth_token`. A token file that other accounts can read is skipped with a
   warning.

To use an OIDC access token instead of a pairing token, also set
`auth_provider = "oidc.<alias>"` under `[connection.wss]`. For file-permission
rules, OIDC, and what the daemon does with the token, see
[Authentication & principals](../security/authentication.md#breaking-change-remote-wss-requires-authentication).

After that, `zerocode --connect wss://<remote-host>:9781` (or plain `zerocode`
with `uri` in config) needs no more flags, and zerocode reconnects
automatically if the connection drops.

## Config reference

The `wss` section:

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Enable the WSS listener |
| `bind` | `0.0.0.0` | Bind address |
| `port` | `9781` | Listen port |
| `cert_path` | (none) | Absolute path to PEM certificate |
| `key_path` | (none) | Absolute path to PEM private key |
