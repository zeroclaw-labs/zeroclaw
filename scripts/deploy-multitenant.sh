#!/bin/bash
# deploy-multitenant.sh
# 在支持 systemd 的 Ubuntu 22.04+ 服务器上执行
#
# 这个脚本将自动化部署一个安全、多租户的 ZeroClaw 环境。
# 特性:
# - 每个用户一个独立的 ZeroClaw 实例
# - 使用 Nginx 作为反向代理，并进行密码保护
# - 使用 Let's Encrypt (Certbot) 自动配置 HTTPS
# - 使用 systemd 管理服务，确保进程健壮性和开机自启
# - 提供一个 zeroclaw-ctl 工具用于简化管理
#
set -e

# --- 可配置变量 ---
USER_COUNT=20
BASE_PORT=8080
# 重要: 脚本执行前请修改这两个变量
DOMAIN=${DOMAIN:-"yourdomain.com"} 
CERTBOT_EMAIL=${CERTBOT_EMAIL:-"your-email@yourdomain.com"} 
# --- 固定路径 ---
INSTALL_DIR="/opt/zeroclaw"
SERVICE_USER="zeroclaw"

echo "🚀 ZeroClaw 多租户安全部署脚本"
echo "=================================="
echo "配置: 4核8GB/75G (推荐)"
echo "用户数: $USER_COUNT"
echo "主域名: $DOMAIN"
echo "证书邮箱: $CERTBOT_EMAIL"
echo ""

# 检查占位符变量是否已修改
if [ "$DOMAIN" == "yourdomain.com" ] || [ "$CERTBOT_EMAIL" == "your-email@yourdomain.com" ]; then
    echo "🚨 警告: 请在执行脚本前修改 DOMAIN 和 CERTBOT_EMAIL 变量！"
    exit 1
fi

# 1. 系统准备
echo "📦 正在准备系统环境 (更新、安装依赖)..."
sudo apt update && sudo apt upgrade -y
sudo apt install -y nginx apache2-utils curl wget tar git ufw python3-certbot-nginx

# 2. 创建服务用户和目录结构
echo "👤 正在创建服务用户 '$SERVICE_USER' 和目录结构..."
sudo useradd -r -s /bin/false $SERVICE_USER 2>/dev/null || true
sudo mkdir -p $INSTALL_DIR/{bin,instances,nginx/htpasswd,scripts,backup}
sudo chown -R $SERVICE_USER:$SERVICE_USER $INSTALL_DIR

# 3. 下载 ZeroClaw 最新版本
echo "⬇️ 正在下载最新的 ZeroClaw 二进制文件..."
cd /tmp
wget -q https://github.com/myhkstar/zeroclaw/releases/latest/download/zeroclaw-x86_64-unknown-linux-gnu.tar.gz -O zeroclaw.tar.gz
tar -xzf zeroclaw.tar.gz
sudo mv zeroclaw $INSTALL_DIR/bin/
sudo chmod +x $INSTALL_DIR/bin/zeroclaw
rm zeroclaw.tar.gz

# 4. 创建用户实例
echo "🏗️ 正在为 $USER_COUNT 个用户创建实例..."
for i in $(seq 1 $USER_COUNT); do
    USER_ID=$(printf "user-%03d" $i)
    PORT=$((BASE_PORT + i - 1))
    USER_DIR="$INSTALL_DIR/instances/$USER_ID"
    sudo mkdir -p $USER_DIR/{tools,workspace,logs}
    PASSWORD=$(openssl rand -base64 12)
    echo "$USER_ID:$PASSWORD" | sudo tee -a $INSTALL_DIR/nginx/initial_credentials.txt > /dev/null
    sudo htpasswd -b -c $INSTALL_DIR/nginx/htpasswd/$USER_ID $USER_ID "$PASSWORD"

    sudo tee $USER_DIR/config.toml > /dev/null <<EOF
[instance]
name = "$USER_ID"
display_name = "Agent $i"
[gateway]
host = "127.0.0.1"
port = $PORT
require_pairing = true
allow_public_bind = false
[logging]
level = "info"
output = "journal" # <<< [阶段 2] 日志输出到 systemd journal
[providers]
google = { api_key = "\${GEMINI_API_KEY}" }
[models]
default = "google/gemini-1.5-flash-latest"
imageDefault = "google/gemini-1.5-pro-latest"
[tools]
enabled = ["fs", "exec", "web_search"]
workspace_path = "$USER_DIR/workspace"
EOF

    sudo tee $USER_DIR/.env > /dev/null <<EOF
GEMINI_API_KEY=\${GEMINI_API_KEY:-""}
USER_ID=$USER_ID
PORT=$PORT
EOF

    sudo tee /etc/nginx/sites-available/$USER_ID > /dev/null <<EOF
server {
    listen 80;
    server_name agent$i.$DOMAIN;
    auth_basic "ZeroClaw Instance $USER_ID";
    auth_basic_user_file $INSTALL_DIR/nginx/htpasswd/$USER_ID;
    location / {
        proxy_pass http://127.0.0.1:$PORT;
        proxy_http_version 1.1;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
    }
}
EOF
    sudo ln -sf /etc/nginx/sites-available/$USER_ID /etc/nginx/sites-enabled/
    sudo chown -R $SERVICE_USER:$SERVICE_USER $USER_DIR
    sudo chmod 600 $USER_DIR/.env
    echo "✅ 实例 $USER_ID 创建完成 (端口: $PORT)"
done

# 5. [阶段 2] 创建 systemd 服务模板
echo "⚙️ 正在创建 systemd 服务模板..."
sudo tee /etc/systemd/system/zeroclaw@.service > /dev/null <<'EOF'
[Unit]
Description=ZeroClaw Instance for %i
After=network.target

[Service]
Type=simple
User=zeroclaw
Group=zeroclaw
WorkingDirectory=/opt/zeroclaw/instances/%i

# Load environment variables from .env file
EnvironmentFile=/opt/zeroclaw/instances/%i/.env

# Start the gateway
ExecStart=/opt/zeroclaw/bin/zeroclaw gateway --config /opt/zeroclaw/instances/%i/config.toml

# Restart policy
Restart=on-failure
RestartSec=10

# Security hardening
PrivateTmp=true
ProtectSystem=full
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload

# 6. [阶段 2] 创建增强版管理工具 (zeroclaw-ctl)
echo "🛠️ 正在创建基于 systemd 的管理工具 'zeroclaw-ctl'..."
sudo tee /usr/local/bin/zeroclaw-ctl > /dev/null <<'EOF'
#!/bin/bash
set -e
INSTALL_DIR="/opt/zeroclaw"
USER_COUNT=20

# Helper to get user ID string
get_user_id() {
    printf "user-%03d" "$1"
}

CMD=$1
NUM=$2

# Function to run command on a range of users
run_on_users() {
    local action=$1
    local start_num=${2:-1}
    local end_num=${3:-$USER_COUNT}

    if [ -n "$NUM" ]; then
        start_num=$NUM
        end_num=$NUM
    fi

    echo "▶️ 正在对 User(s) $start_num-$end_num 执行 '$action'..."
    for i in $(seq $start_num $end_num); do
        local user_id=$(get_user_id $i)
        if [ -d "$INSTALL_DIR/instances/$user_id" ]; then
            sudo systemctl $action zeroclaw@$user_id
            echo "  ✅ $user_id: $action 完成"
        fi
    done
}

case "$CMD" in
    start)
        run_on_users "start"
        ;;
    stop)
        run_on_users "stop"
        ;;
    restart)
        run_on_users "restart"
        ;;
    enable)
        run_on_users "enable"
        echo "✅ 服务已设置为开机自启。"
        ;;
    disable)
        run_on_users "disable"
        echo "⛔️ 服务已禁止开机自启。"
        ;;
    status)
        echo "📊 ZeroClaw 实例状态 (由 systemd 管理)"
        systemctl list-units 'zeroclaw@*' --all
        echo ""
        echo "要查看单个服务的详细状态和日志，请使用: 'zeroclaw-ctl logs <n>'"
        ;;
    pairing)
        echo "🔑 正在从日志中检索所有用户的配对码..."
        echo "=========================================="
        for i in $(seq 1 $USER_COUNT); do
            user_id=$(get_user_id $i)
            # Use journalctl to get logs from systemd
            code=$(sudo journalctl -u zeroclaw@$user_id -n 50 --no-pager | grep "Pairing code" | tail -1 | awk '{print $NF}' || echo "未找到")
            printf "%-12s: %s\n" "$user_id" "$code"
        done
        echo "=========================================="
        ;;
    logs)
        [ -z "$NUM" ] && echo "用法: zeroclaw-ctl logs <用户号(1-20)>" && exit 1
        user_id=$(get_user_id $NUM)
        echo "📜 正在显示 $user_id 的实时日志 (按 Ctrl+C 退出)..."
        sudo journalctl -u zeroclaw@$user_id -f --output cat
        ;;
    password)
        [ -z "$NUM" ] && echo "用法: zeroclaw-ctl password <用户号>" && exit 1
        user_id=$(get_user_id $NUM)
        read -s -p "输入 $user_id 的新密码: " newpass
        echo ""
        sudo htpasswd -b $INSTALL_DIR/nginx/htpasswd/$user_id $user_id "$newpass"
        echo "✅ 密码已更新。"
        ;;
    *)
        echo "ZeroClaw 多租户管理工具 (v2 - systemd)"
        echo "用法: zeroclaw-ctl <命令> [用户号]"
        echo ""
        echo "服务管理 (可指定用户号，默认为全部):"
        echo "  start [n]    启动服务"
        echo "  stop [n]     停止服务"
        echo "  restart [n]  重启服务"
        echo "  enable [n]   设置开机自启"
        echo "  disable [n]  禁止开机自启"
        echo ""
        echo "信息查询:"
        echo "  status       查看所有服务的 systemd 状态"
        echo "  pairing      检索所有用户的配对码"
        echo "  logs <n>     查看指定用户的实时日志"
        echo ""
        echo "用户管理:"
        echo "  password <n> 重置用户的 Web 登录密码"
        ;;
esac
EOF
sudo chmod +x /usr/local/bin/zeroclaw-ctl

# 7. 配置 HTTPS (Let's Encrypt)
echo "🤖 正在使用 Certbot 配置 HTTPS..."
DOMAIN_ARGS=$(for i in $(seq 1 $USER_COUNT); do echo -n "-d agent$i.$DOMAIN "; done)
sudo certbot --nginx $DOMAIN_ARGS --non-interactive --agree-tos -m "$CERTBOT_EMAIL" --redirect
echo "🔒 HTTPS 配置完成。"

# 8. 配置防火墙
echo "🔥 正在配置防火墙 (UFW)..."
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 'Nginx Full' comment 'Nginx HTTP & HTTPS'
sudo ufw allow 22/tcp comment 'SSH'
sudo ufw --force enable

# 9. 测试并重载 Nginx
echo "🚀 正在测试并应用 Nginx 配置..."
sudo nginx -t && sudo systemctl restart nginx

# 10. 显示完成信息
echo ""
echo "🎉 部署完成！所有服务均由 systemd 管理。"
echo "==============================================="
echo ""
echo "🚨 [重要] 初始用户凭据保存在: $INSTALL_DIR/nginx/initial_credentials.txt"
echo "   请在分发给用户后立即删除此文件！"
echo ""
echo "🌐 访问地址 (已启用 HTTPS):"
for i in {1..20}; do
    echo "  User-$(printf "%03d" $i): https://agent$i.$DOMAIN"
done
echo ""
echo "⚙️ 管理命令 (新):"
echo "  zeroclaw-ctl enable    # 将所有服务设为开机自启"
echo "  zeroclaw-ctl start     # 启动所有服务"
echo "  zeroclaw-ctl status    # 查看所有服务状态"
echo "  zeroclaw-ctl pairing   # 查看所有配对码"
echo "  zeroclaw-ctl logs 5    # 查看 user-005 的日志"
echo ""
echo "🔧 下一步关键操作:"
echo "  1. [DNS] 确保 agent1.$DOMAIN 到 agent20.$DOMAIN 的 A 记录已指向本机 IP。"
echo "  2. [API Key] 通知用户编辑 $INSTALL_DIR/instances/user-*/.env 文件，填入他们的 GEMINI_API_KEY。"
echo "  3. [启动] 运行 'zeroclaw-ctl enable' 和 'zeroclaw-ctl start'。"
echo "  4. [安全] 删除 $INSTALL_DIR/nginx/initial_credentials.txt 文件。"
echo ""
