# Deployment Guide — zcplatform

Target: bare Ubuntu 22.04 LTS server, single-host deployment.

---

## 1. Prerequisites

- Ubuntu 22.04+ (x86_64 or arm64)
- 2 GB RAM minimum (4 GB recommended for multiple tenants)
- 20 GB disk minimum
- Docker 24+
- Caddy 2.7+
- A domain with wildcard DNS: `*.example.com → <server-ip>`
- SMTP credentials for OTP delivery
- Open ports: 80, 443 (inbound); 8080 blocked from external

---

## 2. Install Dependencies

```bash
# System packages
sudo apt update && sudo apt install -y \
    build-essential curl git pkg-config \
    libssl-dev ca-certificates

# Rust (if building on server)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Docker
curl -fsSL https://get.docker.com | sudo sh
sudo usermod -aG docker $USER
newgrp docker

# Verify Docker
docker --version   # Docker version 24.x.x

# Caddy
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
    | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
    | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update && sudo apt install caddy

# Verify Caddy
caddy version   # v2.7.x
```

---

## 3. Build zcplatform Binary

```bash
git clone https://github.com/your-org/ZeroClaw.git
cd ZeroClaw

# Build only the platform binary (release profile)
cargo build -p zcplatform --release

# Copy to system path
sudo cp target/release/zcplatform /usr/local/bin/zcplatform
sudo chmod +x /usr/local/bin/zcplatform

zcplatform --version
```

---

## 4. Build ZeroClaw Tenant Docker Image

```bash
# Option A: use zcplatform CLI (reads image config from platform.toml)
zcplatform build-image --config /etc/zcplatform/platform.toml

# Option B: manual docker build
cd ZeroClaw
docker build -f Dockerfile.agent -t zeroclaw-agent:latest .

# Verify image exists
docker image ls zeroclaw-agent
```

The image name must match `[docker] image` in `platform.toml`.

---

## 5. Create platform.toml

```bash
sudo mkdir -p /etc/zcplatform
sudo mkdir -p /var/lib/zcplatform
```

```bash
sudo tee /etc/zcplatform/platform.toml > /dev/null << 'EOF'
[server]
host = "127.0.0.1"
port = 8080
# URL zcplatform is reachable at from Caddy (used in OTP email links)
public_url = "https://platform.example.com"

[database]
path = "/var/lib/zcplatform/platform.db"

[docker]
image = "zeroclaw-agent:latest"
network_internal = "zeroclaw-internal"
network_external = "zeroclaw-external"
port_range_start = 32000
port_range_end   = 33000
uid_range_start  = 200000
uid_range_end    = 299999
data_dir         = "/var/lib/zcplatform/tenants"
vault_key_path   = "/etc/zcplatform/vault.key"

[proxy]
enabled = false
# image = "tinyproxy:latest"
# http_proxy = "http://proxy:8888"

[jwt]
secret = "CHANGE_THIS_TO_A_RANDOM_256BIT_HEX_STRING"
expiry_seconds = 86400

[smtp]
host = "smtp.example.com"
port = 587
username = "noreply@example.com"
password = "smtp-password"
from = "ZeroClaw Platform <noreply@example.com>"
starttls = true

[plans.free]
max_channels          = 2
max_members           = 3
max_messages_per_day  = 500
cpu_limit             = "0.25"
memory_limit_mb       = 256
pids_limit            = 64

[plans.pro]
max_channels          = 10
max_members           = 20
max_messages_per_day  = 10000
cpu_limit             = "1.0"
memory_limit_mb       = 1024
pids_limit            = 128
EOF
```

Generate a real JWT secret:
```bash
openssl rand -hex 32
# paste output into [jwt] secret above
```

---

## 6. Bootstrap

```bash
# Creates: SQLite DB, super-admin user, vault key, Docker networks
sudo zcplatform bootstrap --config /etc/zcplatform/platform.toml \
    --admin-email admin@example.com

# Expected output:
# [bootstrap] Database initialized at /var/lib/zcplatform/platform.db
# [bootstrap] Vault key written to /etc/zcplatform/vault.key (permissions: 0600)
# [bootstrap] Docker network zeroclaw-internal created
# [bootstrap] Docker network zeroclaw-external created
# [bootstrap] Super-admin created: admin@example.com
# [bootstrap] Done. Start the server with: zcplatform serve
```

---

## 7. Configure Caddy

Caddy must handle wildcard subdomains and proxy to zcplatform.

```bash
sudo tee /etc/caddy/Caddyfile > /dev/null << 'EOF'
{
    # Enable Caddy admin API (loopback only — zcplatform injects routes)
    admin localhost:2019
    email acme@example.com
}

# Platform admin UI / API
platform.example.com {
    reverse_proxy 127.0.0.1:8080
}

# Wildcard tenant subdomains
*.example.com {
    tls {
        dns <your-dns-provider> {
            # DNS challenge credentials for wildcard cert
        }
    }
    # Caddy routes per tenant are injected dynamically via admin API
    # zcplatform calls POST http://localhost:2019/config/apps/http/servers/...
    reverse_proxy 127.0.0.1:8080
}
EOF

sudo systemctl reload caddy
sudo systemctl status caddy
```

For wildcard TLS, Caddy requires a DNS challenge provider plugin. Build Caddy with the appropriate plugin or use `xcaddy`:

```bash
xcaddy build --with github.com/caddy-dns/cloudflare
```

---

## 8. Install Systemd Service

```bash
# Copy service file from repository
sudo cp deploy/zcplatform.service /etc/systemd/system/zcplatform.service

# Verify the service file (key fields):
# ExecStart=/usr/local/bin/zcplatform serve --config /etc/zcplatform/platform.toml
# User=zcplatform
# Restart=on-failure

# Create service user
sudo useradd --system --no-create-home --shell /usr/sbin/nologin zcplatform
sudo chown -R zcplatform:zcplatform /var/lib/zcplatform
sudo chown zcplatform:zcplatform /etc/zcplatform/platform.toml
sudo chmod 600 /etc/zcplatform/vault.key
sudo chown zcplatform:zcplatform /etc/zcplatform/vault.key

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable zcplatform
sudo systemctl start zcplatform
sudo systemctl status zcplatform
```

Check logs:
```bash
journalctl -u zcplatform -f
```

---

## 9. Configure SMTP for OTP Emails

SMTP settings are in `[smtp]` in `platform.toml` (configured in step 5). Test delivery:

```bash
zcplatform test-smtp --config /etc/zcplatform/platform.toml \
    --to verify@example.com
# Should deliver a test email within 30 seconds
```

If using Gmail: set `port = 587`, `starttls = true`, use an App Password (not account password).

---

## 10. Verify Deployment

```bash
# 1. Check server is listening
curl -s http://127.0.0.1:8080/health
# { "status": "ok", "version": "x.y.z" }

# 2. Request OTP for admin
curl -s -X POST http://127.0.0.1:8080/auth/otp \
    -H 'Content-Type: application/json' \
    -d '{"email":"admin@example.com"}'
# Check email for 6-digit code

# 3. Verify OTP and get JWT
TOKEN=$(curl -s -X POST http://127.0.0.1:8080/auth/verify \
    -H 'Content-Type: application/json' \
    -d '{"email":"admin@example.com","code":"123456"}' \
    | jq -r .token)

# 4. Create first tenant
curl -s -X POST http://127.0.0.1:8080/admin/tenants \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d '{"name":"acme","plan":"free","owner_email":"admin@example.com"}'

# 5. Check container is running
docker ps | grep zeroclaw-
```

---

## 11. Firewall Rules

```bash
# UFW setup: allow HTTPS/HTTP, block 8080 from external
sudo ufw allow 22/tcp
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw deny 8080/tcp   # zcplatform API — loopback only
sudo ufw deny 2019/tcp   # Caddy admin — loopback only
sudo ufw enable

sudo ufw status
```

Verify zcplatform is NOT reachable externally:
```bash
# From a remote machine — should timeout or refuse
curl -m 5 http://<server-ip>:8080/health
```

---

## 12. Backup Strategy

```bash
# Manual backup (creates timestamped archive: platform-backup-YYYYMMDD.tar.gz)
zcplatform backup --config /etc/zcplatform/platform.toml \
    --output /var/backups/zcplatform/

# Cron: daily backup at 02:00
sudo crontab -e
# Add:
# 0 2 * * * /usr/local/bin/zcplatform backup \
#     --config /etc/zcplatform/platform.toml \
#     --output /var/backups/zcplatform/ >> /var/log/zcplatform-backup.log 2>&1

# Restore
zcplatform restore --config /etc/zcplatform/platform.toml \
    --input /var/backups/zcplatform/platform-backup-20260221.tar.gz
```

Backup archive includes: SQLite DB, vault.key (encrypted), tenant config snapshots. Does NOT include container images or tenant runtime data volumes (back those up separately if needed).

Offsite: rsync or rclone the `/var/backups/zcplatform/` directory to S3 or equivalent. The vault.key inside the archive is already encrypted; the archive itself should also be kept access-controlled.
