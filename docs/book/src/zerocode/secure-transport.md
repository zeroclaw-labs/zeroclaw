# Secure transport: end-to-end configuration

This page is the full configuration reference for connecting a client to a daemon
securely, in three topologies:

1. **Client direct to daemon** - mutual-TLS WSS, no relay.
2. **Daemon to relay** - the daemon keeps an outbound bridge to a nominated relay
   so it is reachable from behind NAT/CGNAT.
3. **Client to relay to daemon** - the client reaches the daemon *through* that
   relay, while the real client<->daemon mTLS still terminates at the daemon.

For the 60-second quickstart see [Remote setup (WSS)](./remote.md); this page is
the deeper, knob-by-knob guide.

## Mental model: two envelopes, one trust boundary

There are two TLS layers, and only one of them is the security boundary:

- **Inner mTLS (the real boundary).** TLS 1.3 only, mutually authenticated. The
  client presents a daemon-issued certificate; the daemon presents its server
  leaf. This is the RPC plane. There is **no** server-only / unauthenticated path
  on it - a client certificate is always required.
- **Outer TLS (a metadata boundary).** When a relay is in the path, the relay
  terminates an outer TLS + WebSocket session and forwards opaque ciphertext. It
  never holds a key that can read the inner RPC. On the direct topology there is
  no outer layer.

Default ports (all configurable):

| Plane | Default | Config |
|-------|---------|--------|
| Daemon WSS (inner mTLS RPC) | `9781` | `[wss].port` |
| Daemon enrollment endpoint | `9782` | `[enroll].port` |
| Relay (outer TLS + WS) | `8443` | relay `--bind` / `[bind]` |

Throughout, `<data_dir>` is the daemon's data directory (typically `~/.zeroclaw`)
and `<config-dir>` is the client's zerocode config directory (`--config-dir`,
typically `~/.zeroclaw`). Config files do not expand `~`; use absolute paths.

---

## Topology 1: client direct to daemon

```
zerocode  ===== mutual-TLS WSS (TLS 1.3) =====>  daemon  [wss] :9781
```

### 1a. Daemon side

Enable the WSS listener. The secure default is to let the daemon **auto-generate
its own CA and server certificate** on first boot, so you do not hand-manage any
TLS material:

```toml
[wss]
enabled = true
# bind = "0.0.0.0"   # default
# port = 9781        # default
# Leave cert_path/key_path empty to auto-generate a server cert under
# <data_dir>/tls/ on first boot. Set them only to bring your own server cert.
```

On first start with `[wss].enabled = true` the daemon writes, under
`<data_dir>/tls/` (directory mode `0700`):

| File | Purpose | Mode |
|------|---------|------|
| `ca.crt` | Per-daemon CA certificate (public) | default umask |
| `ca.key` | CA private key (signs client certs) | `0600` |
| `server.crt` | WSS server leaf (SANs `localhost`, `127.0.0.1`) | default umask |
| `server.key` | WSS server private key | `0600` |

Private keys are written `0600`; the public certificates use the process umask.
The `tls/` directory itself is `0700`.

The CA is **never silently rotated**: if `ca.crt` and `ca.key` exist they are
reused. Auto-generated CA lifetime is 10 years; the server leaf is ~27 months;
issued client certs are 30 days.

Open the port (`sudo ufw allow 9781/tcp`) and start the daemon. You should see a
log line that the WSS listener is up on `0.0.0.0:9781`.

The client now needs a certificate. There are two ways to get one.

### 1b. Client side - option A: enrollment (recommended)

Enrollment gives a certless client its first certificate over a pairing-gated,
server-authenticated endpoint, with no manual cert handling. Turn it on:

```toml
[enroll]
enabled = true
# bind = "0.0.0.0"   # default
# port = 9782        # default
# Requires [wss] enabled and a daemon CA key (auto-generated above, or BYO+key).
# If the CA key is absent the endpoint fails closed and certs must be provisioned
# out of band.
```

The daemon prints a one-time **pairing code** and a **short-auth-string (SAS)** to
its console/log on start. Then, on the workstation:

```sh
# Interactive: a certless client auto-enrolls on first connect.
zerocode --connect wss://<remote-host>:9781

# Or explicitly / non-interactively:
zerocode --enroll --connect wss://<remote-host>:9781
```

zerocode prompts for the pairing code, generates a P-256 key and CSR **locally
(the private key never leaves the device)**, and shows the SAS. Confirm the SAS
matches the one the daemon printed (this catches a man-in-the-middle CA), and it
caches, under `<config-dir>/tls/`:

| File | Purpose | Mode |
|------|---------|------|
| `client.crt` | Issued client certificate | default umask |
| `client.key` | Client private key (generated locally) | `0600` |
| `ca.crt` | Daemon CA chain, pinned for the RPC plane | default umask |
| `profile.json` | Cached `device_id`, `not_after`, relay profile | default umask |

Every later run is zero-config (`zerocode --connect wss://<remote-host>:9781`,
or just `zerocode` if `uri` is in config). The cert auto-renews at ~50% of its
lifetime (~15 days) over the live mTLS session; a revoked cert cannot self-renew.

Enrollment endpoint defaults: `--enroll-host` defaults to `--connect`'s host;
`--enroll-port` defaults to `9782`.

**Migrating an existing fleet.** To turn on mTLS without distributing pairing
codes, open a time-boxed code-less window that self-closes by wall-clock:

```toml
[enroll]
enabled = true
allow_unpaired_enrollment = "2026-07-01T00:00:00Z"   # RFC3339 deadline
```

The daemon logs a loud warning each start while the window is open; a malformed
deadline is rejected at startup rather than silently treated as closed.

### 1c. Client side - option B: operator-issued certificate

If you would rather mint a cert on the daemon and copy it out:

```sh
# On the daemon host. --out-dir also writes a drop-in ca.crt/client.crt/client.key.
zeroclaw security issue-client-cert --name my-laptop --out-dir /tmp/my-laptop-tls
# add --force to overwrite an existing cert for this name
```

Copy the three files to the client's `<config-dir>/tls/` as `ca.crt`,
`client.crt`, `client.key` (then `zerocode --connect wss://host:9781` finds them
automatically), or point at them explicitly:

```sh
zerocode --connect wss://<remote-host>:9781 \
  --tls-ca-cert  /path/ca.crt \
  --tls-client-cert /path/client.crt \
  --tls-client-key  /path/client.key
```

Equivalent config (so plain `zerocode` works):

```toml
[connection.wss]
uri = "wss://<remote-host>:9781"

[connection.wss.tls]
ca_cert_path     = "/abs/path/ca.crt"
client_cert_path = "/abs/path/client.crt"
client_key_path  = "/abs/path/client.key"
```

> A certless client that reaches the WSS plane without enrolling gets an
> actionable "enroll first" message (and the daemon logs the rejected
> un-migrated client) - never a silent hang. `--tls-skip-verify` only relaxes
> *server* verification for a self-signed dev daemon; the client certificate is
> still required.

---

## Topology 2: daemon to relay

```
daemon  ====== outbound: register + bridge ======>  relay  :8443
[relay]                                             (blind forwarder)
```

The daemon dials the relay, proves a stable Ed25519 identity, and registers a
**node-id**. Clients later dial that node-id (Topology 3). The relay only ever
forwards ciphertext.

### 2a. Run the relay (zerorelay)

Configure with `relay.toml` (see `apps/zerorelay/relay.example.toml`); every CLI
flag overrides the matching file value. The `[admission]` section hot-reloads on
`SIGHUP`.

```toml
# relay.toml
bind = "0.0.0.0:8443"

[tls]
# Omit cert/key to SELF-PROVISION an outer TLS cert into dir on first run (no
# openssl). Set sans to the relay's public hostname(s)/IP(s).
dir  = "/data/tls"
sans = ["relay.example.com"]
# Or bring your own (e.g. a public-CA cert):
# cert = "/etc/zerorelay/fullchain.pem"
# key  = "/etc/zerorelay/privkey.pem"

[admission]
# "open" admits any signed daemon (subject to the deny list); "allowlist" admits
# only listed daemon pubkey fingerprints. Deny always wins.
mode = "open"
allow = []
deny  = []
# Optional shared-secret gate a daemon must present at registration:
# relay_token = "change-me"

[limits]
max_conns_per_node     = 256
idle_timeout_secs      = 300
lease_ttl_secs         = 300
accept_burst_per_ip    = 30
accept_rate_per_ip     = 10.0
connect_burst_per_node = 60
connect_rate_per_node  = 20.0
```

Run it:

```sh
zerorelay --config /etc/zerorelay/relay.toml
# equivalently, flags only:
zerorelay --bind 0.0.0.0:8443 --tls-san relay.example.com
```

When `--tls-cert`/`--tls-key` are omitted the relay self-provisions a CA + server
cert under the TLS dir (resolution order: `$ZERORELAY_DATA_DIR/tls`, else
`$HOME/.zerorelay/tls`, else `./zerorelay/tls`); `localhost` and `127.0.0.1` are
always in the SANs. The self-provisioned `ca.crt` is what a daemon/client trusts
for the relay's outer TLS.

**Admission.** `open` mode plus an optional `relay_token` is the simplest access
control. `allowlist` mode keys on the daemon's registration pubkey fingerprint
(SHA-256 hex of the Ed25519 key at the daemon's `<data_dir>/relay/registration.key`);
add fingerprints to `allow` (and reload with `kill -HUP <pid>`). A node-id is
bound to its first registrant's pubkey, so a different key cannot hijack a live
node-id (it gets `node_taken`).

**Docker.** `apps/zerorelay/Dockerfile` runs distroless with
`CMD ["--config", "/etc/zerorelay/relay.toml"]` and a shell-less
`zerorelay healthcheck --addr` HEALTHCHECK; `compose.yaml` mounts a volume at
`/data` so the self-provisioned TLS persists. Expose `8443`.

### 2b. Point the daemon at the relay

```toml
[wss]
enabled = true          # REQUIRED: the relay forwards to the local WSS listener

[relay]
enabled = true
url = "relay.example.com:8443"
# node_id: leave EMPTY (recommended) to auto-mint + persist a random 128-bit
# capability at <data_dir>/relay/node_id. Set it only to pin a specific id.
# token = "change-me"   # must match the relay's [admission].relay_token, if set

# Trust for the relay's OUTER certificate - pick ONE:
relay_ca_path = "/path/to/relay/ca.crt"   # trust the relay's (self-signed) CA
# tofu = true                              # OR pin the relay leaf on first use
# relay_insecure = true                    # OR skip outer verification (dev only)
# (leave all three unset to use built-in public roots, for a public-CA relay)
```

`[relay]` requires `[wss]` enabled (the relay bridges to `127.0.0.1:<wss.port>`),
and fails closed if `url` is empty. On start the daemon logs the node-id with the
hint *"give clients this as --relay-node"*; you can also read it from
`<data_dir>/relay/node_id`. The daemon's stable registration key is created at
`<data_dir>/relay/registration.key` (`0600`).

Outer-cert trust precedence (highest first): `relay_insecure` > a stored pin at
`<data_dir>/relay/relay_pin` (explicit or TOFU) > `tofu` > `relay_ca_path` >
public roots. With `tofu = true` the observed relay leaf fingerprint is pinned to
`<data_dir>/relay/relay_pin`, and enrollment hands that same pin to clients so
they pin the identical leaf.

### 2c. (optional) node-id rotation and outer mTLS

```toml
[relay]
node_id_rotation_days = 30   # auto-rotate the auto-minted id every N days (0 = never)
```

Rotation mints a fresh id, runs it alongside the old one for a 10-minute grace
window so in-flight clients are not cut off, then retires the old id; the new id
reaches clients in-band on their next certificate renewal. Force one now with
`zeroclaw security relay-rotate-node-id` (auto-mint mode only; a pinned `node_id`
is never rotated).

For a relay that authenticates daemons on the outer layer too, set the
relay's `[admission].outer_client_auth = "required"` + `outer_client_ca`, and on
the daemon `[relay].outer_client_cert` / `outer_client_key`. This is additive on
the outer TLS and never touches the inner mTLS.

---

## Topology 3: client to relay to daemon

```
zerocode  ==outer TLS+WS==>  relay  ==forwards ciphertext==>  daemon
   \________________ inner mutual-TLS (TLS 1.3) terminates here _______________/
```

This composes Topologies 1 and 2: the client needs an inner client certificate
(enroll, as in 1b) **and** the relay coordinates (address, node-id, and trust for
the relay's outer cert).

### 3a. The easy path: enrollment carries the relay profile

When the daemon has `[relay]` configured, its enrollment response includes a
**relay profile** (`relay_url`, `node_id`, and the relay's leaf `relay_cert_pin`).
So a single enrollment provisions everything:

```sh
zerocode --enroll --connect wss://<daemon-host>:9781
```

zerocode caches the inner cert **and** the relay profile in
`<config-dir>/tls/profile.json`. Later, plain `zerocode` reaches the daemon
through the relay with no flags - it already knows the relay address, the
node-id, and the pin.

### 3b. The manual path

Give the client the relay coordinates explicitly. The inner cert still comes from
enrollment or `--tls-*` (Topology 1):

```sh
zerocode \
  --relay      relay.example.com:8443 \
  --relay-node <node-id-from-daemon-log> \
  --relay-ca   /path/to/relay/ca.crt
# inner mTLS material: from <config-dir>/tls (after enrolling), or pass --tls-* flags
```

Choose exactly one trust mode for the relay's outer cert, mirroring the daemon:

| Flag | Meaning |
|------|---------|
| `--relay-ca <pem>` | Trust the relay's (self-signed) CA |
| `--relay-pin <sha256>` | Pin the relay's outer leaf (usually delivered at enrollment) |
| `--relay-tofu` | Trust on first use; persist the pin to `<config-dir>/relay/relay_pin` |
| `--relay-insecure` | Skip outer verification (dev/self-signed only) |
| (none) | Use built-in public roots (public-CA relay) |
| `--relay-host <name>` | Override the expected outer-cert SAN (defaults to the `--relay` host) |

Config equivalent (so plain `zerocode` works):

```toml
[connection.wss]
relay_url  = "relay.example.com:8443"
relay_node = "<node-id>"
```

> The relay's outer trust (`--relay-ca` / `--relay-pin` / `--relay-tofu` /
> `--relay-insecure`) is supplied by **flags or the cached enrollment pin**, not
> by `[connection.wss]` keys.

### 3c. Direct-first with relay fallback

If you give the client **both** a direct address and a relay, it prefers the
direct path and falls back to the relay, then re-probes and migrates back:

```sh
zerocode --connect wss://<daemon-host>:9781 \
         --relay relay.example.com:8443 --relay-node <node-id>
```

Tuning (in `[connection.wss]`):

| Key | Default | Meaning |
|-----|---------|---------|
| `direct_attempts` | `2` | Direct attempts before falling back to the relay |
| `direct_timeout_secs` | `3` | Per-attempt direct-connect timeout |
| `reprobe_secs` | `30` | Re-probe cadence to migrate back to direct (`0` disables) |

In relay-only mode (no `--connect`/`uri`), the inner WSS URL defaults to
`wss://127.0.0.1:9781` because the inner mTLS terminates at the daemon's loopback
listener; the relay address is only the TCP dial target.

---

## Configuration reference

### Daemon `[wss]`

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Enable the mutual-TLS WSS listener |
| `bind` | `0.0.0.0` | Bind address |
| `port` | `9781` | Listen port |
| `cert_path` | (empty) | BYO server cert; empty auto-generates under `<data_dir>/tls/` |
| `key_path` | (empty) | BYO server key; empty auto-generates |

### Daemon `[wss.client_auth]` (optional; mTLS is always on either way)

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Use a BYO CA; when false the daemon uses its auto-generated CA |
| `ca_cert_path` | (empty) | PEM CA used to verify client certs (BYO mode) |
| `pinned_certs` | `[]` | If non-empty, only client certs matching these SHA-256 fingerprints are accepted |
| `crl_path` | (empty) | Revoked-fingerprint file; empty uses the ledger-materialized `<data_dir>/tls/revoked` |

### Daemon `[enroll]`

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Enable the enrollment endpoint (requires `[wss]` + a CA key) |
| `bind` | `0.0.0.0` | Bind address |
| `port` | `9782` | Listen port |
| `allow_unpaired_enrollment` | (empty) | RFC3339 deadline for code-less enrollment; empty = pairing-code required |

### Daemon `[relay]`

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Enable the relay bridge (requires `[wss]`) |
| `url` | (empty) | Relay address `host:port`; required when enabled |
| `node_id` | (empty) | Empty auto-mints + persists a 128-bit id; set to pin one |
| `token` | (empty, secret) | Shared-secret presented at registration |
| `relay_ca_path` | (empty) | PEM CA for the relay's outer cert; empty uses public roots |
| `relay_host` | (empty) | Expected outer-cert SAN; empty derives from `url` |
| `relay_insecure` | `false` | Skip outer-cert verification (dev only) |
| `tofu` | `false` | Pin the relay leaf on first use to `<data_dir>/relay/relay_pin` |
| `outer_client_cert` | (empty) | Daemon's outer-mTLS client cert for relay admission |
| `outer_client_key` | (empty) | Key for `outer_client_cert` |
| `node_id_rotation_days` | `0` | Auto-rotate the auto-minted node-id every N days (0 = never) |

### Relay `relay.toml`

| Section.key | Default | Description |
|-------------|---------|-------------|
| `bind` | `0.0.0.0:8443` | Listen address (daemon + client) |
| `[tls].cert` / `.key` | (self-provision) | Outer TLS identity; omit both to self-provision |
| `[tls].dir` | data dir `/tls` | Where the self-provisioned cert is written |
| `[tls].sans` | `[]` | Extra SANs (`localhost`, `127.0.0.1` always included) |
| `[admission].mode` | `open` | `open` or `allowlist` |
| `[admission].allow` / `.deny` | `[]` | Daemon pubkey fingerprints (deny wins) |
| `[admission].relay_token` | (none) | Optional shared-secret gate |
| `[admission].outer_client_auth` | `off` | `off` / `optional` / `required` (outer mTLS) |
| `[admission].outer_client_ca` | (none) | PEM CA for outer client certs |
| `[admission].route_by_client_cert` | `false` | Route by the outer cert CN's node-id |
| `[limits].max_conns_per_node` | `256` | Simultaneous client conns per node-id |
| `[limits].idle_timeout_secs` | `300` | Drop idle client conns after N seconds |
| `[limits].lease_ttl_secs` | `300` | Lease TTL advertised at registration |
| `[limits].accept_burst_per_ip` / `accept_rate_per_ip` | `30` / `10.0` | Per-IP handshake token bucket |
| `[limits].connect_burst_per_node` / `connect_rate_per_node` | `60` / `20.0` | Per-node connect token bucket |

### zerorelay CLI (overrides `relay.toml`)

`--config` `--bind` `--tls-cert` `--tls-key` `--tls-dir` `--tls-san` (repeatable)
`--registration-mode` `--allow` (repeatable) `--deny` (repeatable) `--relay-token`
`--max-conns-per-node` `--idle-timeout-secs` `--lease-ttl-secs` `--status-file`.
Subcommands: `healthcheck [--addr 127.0.0.1:8443]`, `status --file <path>`.

### zerocode `[connection.wss]` and CLI

| `[connection.wss]` key | Default | CLI override |
|------------------------|---------|--------------|
| `uri` | (none) | `--connect` |
| `relay_url` | (none) | `--relay` (requires `--relay-node`) |
| `relay_node` | (none) | `--relay-node` (requires `--relay`) |
| `direct_attempts` | `2` | - |
| `direct_timeout_secs` | `3` | - |
| `reprobe_secs` | `30` | - |

| `[connection.wss.tls]` key | Default | CLI override |
|----------------------------|---------|--------------|
| `ca_cert_path` | `<config-dir>/tls/ca.crt` | `--tls-ca-cert` |
| `client_cert_path` | `<config-dir>/tls/client.crt` | `--tls-client-cert` (requires key) |
| `client_key_path` | `<config-dir>/tls/client.key` | `--tls-client-key` (requires cert) |
| `skip_verify` | `false` | `--tls-skip-verify` |

Relay outer-trust and enrollment are CLI/cache only: `--relay-ca` `--relay-host`
`--relay-insecure` `--relay-pin` `--relay-tofu` `--relay-client-cert`
`--relay-client-key` `--enroll` `--enroll-host` `--enroll-port`.

## File layout

**Daemon `<data_dir>/`:**

```
tls/ca.crt  tls/ca.key            per-daemon CA (key 0600)
tls/server.crt  tls/server.key    WSS server leaf (key 0600)
tls/ledger.db                     issued-cert ledger (SQLite)
tls/revoked                       revoked fingerprints (handshake-checked)
relay/registration.key            Ed25519 relay identity (0600)
relay/node_id                     auto-minted node-id
relay/relay_pin                   pinned relay outer-leaf fingerprint (TOFU)
```

**Client `<config-dir>/`:**

```
tls/client.crt  tls/client.key    client identity (key 0600)
tls/ca.crt                         pinned daemon CA
tls/profile.json                   device_id, not_after, cached relay profile
relay/relay_pin                    relay outer-leaf pin (--relay-tofu)
```

**Relay `<tls-dir>/`:** `ca.crt`, `server.crt`, `server.key` (self-provisioned).

## Verifying and troubleshooting

- **Daemon up:** look for the WSS listener log on `0.0.0.0:9781` and, with a relay,
  the `node_id` log line.
- **Certless connect refused:** expected on the mTLS plane - enroll first
  (Topology 1b). The message is actionable, not a hang.
- **Relay reachable:** `zerorelay healthcheck --addr <host>:8443` exits 0.
- **Relay metrics:** run with `--status-file <path>`, then
  `zerorelay status --file <path>` (counts only, never payloads).
- **Revoke a lost device:** revoking in the daemon ledger materializes
  `<data_dir>/tls/revoked`; the cert is refused at its next handshake and cannot
  self-renew.
- **SAS mismatch at enrollment:** the client refuses to persist the cert and
  aborts. A mismatch means the CA you received is not the daemon's - investigate a
  possible man-in-the-middle before retrying.

There is a self-contained end-to-end harness at `scripts/dev/mtls-relay-testbed.sh`
that boots a daemon + relay, issues a cert, enrolls over the wire, and exercises
all three topologies; read it as a worked example.
