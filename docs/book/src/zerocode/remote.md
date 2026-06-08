# Remote setup (WSS)

Connect zerocode on your workstation to a daemon running on another machine
(Raspberry Pi, home server, VPS, etc.).

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

2. **Enable WSS.** Set the `wss` config through the [Config](./config.md) pane (or the gateway / `zeroclaw config set`). Use absolute paths — the config does not expand `~`.

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

The `[wss]` section in `config.toml`:

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Enable the WSS listener |
| `bind` | `0.0.0.0` | Bind address |
| `port` | `9781` | Listen port |
| `cert_path` | (none) | Absolute path to PEM certificate |
| `key_path` | (none) | Absolute path to PEM private key |
