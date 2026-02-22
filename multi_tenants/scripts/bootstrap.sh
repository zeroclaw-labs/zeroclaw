#!/usr/bin/env bash
set -euo pipefail

# ZeroClaw Multi-Tenant Platform Bootstrap Script
# Target: Ubuntu 22.04+ / Debian 12+ (amd64 or arm64)
# Usage: sudo bash bootstrap.sh --domain example.com --admin-email admin@example.com

DOMAIN=""
ADMIN_EMAIL=""
DATA_DIR="/opt/zcplatform/data"
INSTALL_DIR="/opt/zcplatform"

while [[ $# -gt 0 ]]; do
  case $1 in
    --domain) DOMAIN="$2"; shift 2 ;;
    --admin-email) ADMIN_EMAIL="$2"; shift 2 ;;
    --data-dir) DATA_DIR="$2"; shift 2 ;;
    --install-dir) INSTALL_DIR="$2"; shift 2 ;;
    -h|--help)
      echo "Usage: sudo bash bootstrap.sh --domain DOMAIN --admin-email EMAIL"
      echo ""
      echo "Options:"
      echo "  --domain        Base domain for tenant subdomains (required)"
      echo "  --admin-email   Super-admin email address (required)"
      echo "  --data-dir      Data directory (default: /opt/zcplatform/data)"
      echo "  --install-dir   Install directory (default: /opt/zcplatform)"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

if [[ -z "$DOMAIN" || -z "$ADMIN_EMAIL" ]]; then
  echo "Error: --domain and --admin-email are required"
  echo "Usage: sudo bash bootstrap.sh --domain example.com --admin-email admin@example.com"
  exit 1
fi

if [[ $EUID -ne 0 ]]; then
  echo "Error: This script must be run as root (use sudo)"
  exit 1
fi

echo "=== ZeroClaw Platform Bootstrap ==="
echo "Domain:  $DOMAIN"
echo "Admin:   $ADMIN_EMAIL"
echo "Data:    $DATA_DIR"
echo "Install: $INSTALL_DIR"
echo ""

# 1. System packages
echo ">>> [1/12] Installing system packages..."
apt-get update -qq
apt-get install -y -qq \
  curl wget git build-essential pkg-config \
  ca-certificates gnupg lsb-release

# 2. Docker
echo ">>> [2/12] Installing Docker..."
if ! command -v docker &>/dev/null; then
  curl -fsSL https://get.docker.com | sh
  systemctl enable docker
  systemctl start docker
else
  echo "    Docker already installed"
fi
docker --version

# 3. Caddy
echo ">>> [3/12] Installing Caddy..."
if ! command -v caddy &>/dev/null; then
  apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | \
    gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | \
    tee /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -qq
  apt-get install -y -qq caddy
else
  echo "    Caddy already installed"
fi
caddy version

# 4. Rust toolchain
echo ">>> [4/12] Installing Rust..."
if ! command -v rustc &>/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
else
  echo "    Rust already installed"
fi
rustc --version

# 5. Create directories
echo ">>> [5/12] Creating directories..."
mkdir -p "$INSTALL_DIR" "$DATA_DIR/tenants" "$DATA_DIR/backups"

# 6. Clone and build ZeroClaw
echo ">>> [6/12] Building ZeroClaw agent..."
if [[ ! -d "$INSTALL_DIR/zeroclaw" ]]; then
  git clone https://github.com/zeroclaw-labs/zeroclaw.git "$INSTALL_DIR/zeroclaw"
fi
cd "$INSTALL_DIR/zeroclaw"
git pull --ff-only
cargo build --release --locked
cp target/release/zeroclaw "$INSTALL_DIR/zeroclaw-bin"

# 7. Build zcplatform
echo ">>> [7/12] Building zcplatform..."
cargo build --release --locked -p zcplatform
cp target/release/zcplatform "$INSTALL_DIR/zcplatform-bin"

# 8. Build SPA (if Node.js available)
if command -v node &>/dev/null; then
  echo ">>> [7b/12] Building admin SPA..."
  cd platform/web && npm ci && npm run build && cd ../..
fi

# 9. Build tenant Docker image
echo ">>> [8/12] Building tenant Docker image..."
cd platform/docker
cp "$INSTALL_DIR/zeroclaw-bin" ./zeroclaw
mkdir -p tenant-config
docker build -f Dockerfile.tenant -t zeroclaw-tenant:latest .
rm -f zeroclaw
cd "$INSTALL_DIR/zeroclaw"

# 10. Build egress proxy image
echo ">>> [9/12] Building egress proxy image..."
cd platform/docker
docker build -f Dockerfile.egress -t zcplatform-egress:latest .
cd "$INSTALL_DIR"

# 11. Generate platform config
echo ">>> [10/12] Generating platform config..."
JWT_SECRET=$(openssl rand -hex 32)
cat > "$INSTALL_DIR/platform.toml" <<TOMLEOF
host = "127.0.0.1"
port = 8080
database_path = "$DATA_DIR/platform.db"
master_key_path = "$DATA_DIR/master.key"
data_dir = "$DATA_DIR/tenants"
docker_image = "zeroclaw-tenant:latest"
docker_network = "zcplatform-internal"
port_range = [10001, 10999]
uid_range = [10001, 10999]
domain = "$DOMAIN"
caddy_api_url = "http://localhost:2019"
jwt_secret = "$JWT_SECRET"

[plans.free]
max_messages_per_day = 100
max_channels = 2
max_members = 3
memory_mb = 256
cpu_limit = 0.5

[plans.starter]
max_messages_per_day = 1000
max_channels = 5
max_members = 10
memory_mb = 384
cpu_limit = 0.75

[plans.pro]
max_messages_per_day = 10000
max_channels = 10
max_members = 20
memory_mb = 512
cpu_limit = 1.0
TOMLEOF
chmod 600 "$INSTALL_DIR/platform.toml"

# 12. Bootstrap platform
echo ">>> [11/12] Bootstrapping platform (DB + keys + admin)..."
"$INSTALL_DIR/zcplatform-bin" bootstrap \
  --config "$INSTALL_DIR/platform.toml" \
  --admin-email "$ADMIN_EMAIL"

# 13. Install systemd service
echo ">>> [12/12] Installing systemd service..."
cat > /etc/systemd/system/zcplatform.service <<SVCEOF
[Unit]
Description=ZeroClaw Multi-Tenant Platform
Documentation=https://github.com/zeroclaw-labs/zeroclaw
After=network.target docker.service
Requires=docker.service

[Service]
Type=simple
User=root
WorkingDirectory=$INSTALL_DIR
ExecStart=$INSTALL_DIR/zcplatform-bin serve --config $INSTALL_DIR/platform.toml
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=$DATA_DIR
ProtectHome=true
PrivateTmp=true
StandardOutput=journal
StandardError=journal
SyslogIdentifier=zcplatform
Environment=RUST_LOG=zcplatform=info

[Install]
WantedBy=multi-user.target
SVCEOF

systemctl daemon-reload
systemctl enable zcplatform
systemctl start zcplatform

# 14. Configure Caddy
echo ">>> Configuring Caddy..."
cat > /etc/caddy/Caddyfile <<CADDYEOF
{
    admin localhost:2019
}

*.${DOMAIN}, ${DOMAIN} {
    tls {
        dns cloudflare {env.CLOUDFLARE_API_TOKEN}
    }

    @api host api.${DOMAIN}
    handle @api {
        reverse_proxy localhost:8080
    }

    handle {
        respond "Not Found" 404
    }
}
CADDYEOF

systemctl enable caddy

echo ""
echo "=== Bootstrap Complete ==="
echo ""
echo "Platform API: https://api.${DOMAIN}"
echo "Admin email:  ${ADMIN_EMAIL}"
echo ""
echo "Next steps:"
echo "  1. Set CLOUDFLARE_API_TOKEN in /etc/caddy/caddy.env"
echo "  2. Restart Caddy: systemctl restart caddy"
echo "  3. Check logs: journalctl -u zcplatform -f"
