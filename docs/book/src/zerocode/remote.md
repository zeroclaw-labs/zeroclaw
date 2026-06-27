# Remote setup (WSS)

Connect zerocode on your workstation to a daemon running on another machine
(Raspberry Pi, home server, VPS, etc.).

> **The WSS plane is mutually authenticated (mTLS).** Every client presents a
> certificate; there is no server-only / unauthenticated path. The easy way to
> get a client certificate is **enrollment** (below) - you do not hand-manage
> certs. The legacy `--tls-skip-verify` flow further down still works for a
> self-signed dev daemon but only relaxes *server* verification; the client
> certificate is still required.

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
never a silent hang.

### Migrating an existing fleet

If you already run remote clients and are turning on mTLS, open a time-boxed
**code-less enrollment window** so they can migrate without distributing pairing
codes, then let it self-close:

```toml
[enroll]
enabled = true
# Clients may enroll WITHOUT a pairing code until this RFC3339 deadline. The
# window closes itself by wall-clock; clear this line to close it early.
allow_unpaired_enrollment = "2026-07-01T00:00:00Z"
```

While the window is open the daemon logs a loud warning each start. A malformed
deadline is rejected at startup rather than silently treated as closed. A
**revoked** certificate is refused at the handshake (driven by the issued-cert
ledger), so revoking a lost device takes effect on its next connection.

## On the remote host (daemon side)

1. **Generate a self-signed TLS certificate:**

   <div class="os-tabs-src">

   #### sh

   ```sh
   openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
     -keyout ~/.zeroclaw/wss.key \
     -out ~/.zeroclaw/wss.cert \
     -days 3650 -nodes -subj '/CN=zeroclaw'
   ```

   </div>

2. **Enable WSS.** Set the `wss` config through the [Config](./config.md) pane (or the gateway / `zeroclaw config set`). Use absolute paths; the config does not expand `~`.

3. **Open the firewall port:**

   <div class="os-tabs-src">

   #### sh

   ```sh
   sudo ufw allow 9781/tcp
   ```

   </div>

   The default WSS port is **9781**. Change it with `port = <number>` in the `[wss]` section.

4. **Start (or restart) the daemon:**

   <div class="os-tabs-src">

   #### sh

   ```sh
   zeroclaw daemon
   ```

   </div>

   You should see a log line confirming the WSS listener started on `0.0.0.0:9781`.

## On your workstation (zerocode side)

1. **Connect with TLS verification skipped:**

   <div class="os-tabs-src">

   #### sh

   ```sh
   zerocode --connect wss://<remote-ip>:9781 --tls-skip-verify
   ```

   </div>

   `--tls-skip-verify` is required for self-signed certificates. The HMAC session signing still authenticates the connection.

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
