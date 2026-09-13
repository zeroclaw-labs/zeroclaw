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

## Enrollment (recommended)

The first time you connect a certless client interactively, zerocode enrolls
automatically:

```sh
zerocode --connect wss://<remote-host>:9781
```

It prompts for the daemon's one-time **pairing code** (printed in the daemon's
log on start), shows a **short-auth-string (SAS)** to confirm against the daemon
console (so a man-in-the-middle CA is caught), then fetches and caches a client
certificate under `<config-dir>/tls`. Later runs are zero-config, and the cert
auto-renews at ~50% of its lifetime. To enroll non-interactively use
`zerocode --enroll --connect wss://<remote-host>:9781`.

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

That's it. zerocode reconnects automatically if the connection drops.

## Config reference

The `wss` section:

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Enable the WSS listener |
| `bind` | `0.0.0.0` | Bind address |
| `port` | `9781` | Listen port |
| `cert_path` | (none) | Absolute path to PEM certificate |
| `key_path` | (none) | Absolute path to PEM private key |
