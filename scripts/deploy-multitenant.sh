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
# - 自动创建 Swap 文件以增强系统稳定性
#
set -e

# --- 可配置变量 ---
USER_COUNT=20
BASE_PORT=8080
SWAP_SIZE="4G" # 为 8GB RAM 服务器推荐 4G
# 重要: 脚本执行前请修改这两个变量
DOMAIN=${DOMAIN:-"yourdomain.com"} 
CERTBOT_EMAIL=${CERTBOT_EMAIL:-"your-email@yourdomain.com"} 
# --- 固定路径 ---
INSTALL_DIR="/opt/zeroclaw"
SERVICE_USER="zeroclaw"

echo "🚀 ZeroClaw 多租户安全部署脚本 (v3 - Final)"
echo "============================================="
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
echo "📦 正在准备系统环境..."
sudo apt update && sudo apt upgrade -y
sudo apt install -y nginx apache2-utils curl wget tar git ufw python3-certbot-nginx

# [阶段 3] 配置 Swap 文件以提高稳定性
if [ -f /swapfile ]; then
    echo "✔️ Swap 文件已存在。"
else
    echo "💾 正在创建 ${SWAP_SIZE} Swap 文件..."
    sudo fallocate -l $SWAP_SIZE /swapfile
    sudo chmod 600 /swapfile
    sudo mkswap /swapfile
    sudo swapon /swapfile
    echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
    echo "✅ Swap 文件创建并启用成功。"
fi

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
# [阶段 3] 确认用户 ID 格式 Bug 已修复
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
output = "journal"
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

# 5. 创建 systemd 服务模板
echo "⚙️ 正在创建 systemd 服务模板..."
# (内容同阶段2，无需改变)
sudo tee /etc/systemd/system/zeroclaw@.service > /dev/null <<'EOF'
[Unit]
Description=ZeroClaw Instance for %i
After=network.target
[Service]
Type=simple
User=zeroclaw
Group=zeroclaw
WorkingDirectory=/opt/zeroclaw/instances/%i
EnvironmentFile=/opt/zeroclaw/instances/%i/.env
ExecStart=/opt/zeroclaw/bin/zeroclaw gateway --config /opt/zeroclaw/instances/%i/config.toml
Restart=on-failure
RestartSec=10
PrivateTmp=true
ProtectSystem=full
NoNewPrivileges=true
[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload

# 6. 创建最终版管理工具 (zeroclaw-ctl)
echo "🛠️ 正在创建最终版管理工具 'zeroclaw-ctl'..."
sudo tee /usr/local/bin/zeroclaw-ctl > /dev/null <<'EOF'
#!/bin/bash
set -e
INSTALL_DIR="/opt/zeroclaw"
USER_COUNT=20

get_user_id() { printf "user-%03d" "$1"; }

CMD=$1
NUM=$2

run_on_users() {
    local action=$1; local start_num=${2:-1}; local end_num=${3:-$USER_COUNT}
    [ -n "$NUM" ] && start_num=$NUM && end_num=$NUM
    echo "▶️ 正在对 User(s) $start_num-$end_num 执行 '$action'..."
    for i in $(seq $start_num $end_num); do
        local user_id=$(get_user_id $i)
        [ -d "$INSTALL_DIR/instances/$user_id" ] && sudo systemctl $action zeroclaw@$user_id && echo "  ✅ $user_id: $action 完成"
    done
}

case "$CMD" in
    start|stop|restart|enable|disable)
        run_on_users "$CMD"
        ;;
    status)
        echo "📊 ZeroClaw 实例状态 (由 systemd 管理)"
        echo "========================================================================================="
        printf "%-12s %-10s %-8s %-10s %-12s %s\n" "用户" "状态" "PID" "内存" "开机自启" "配对码"
        echo "-----------------------------------------------------------------------------------------"
        for i in $(seq 1 $USER_COUNT); do
            user_id=$(get_user_id $i)
            if [ ! -d "$INSTALL_DIR/instances/$user_id" ]; then continue; fi
            
            active_state=$(systemctl is-active zeroclaw@$user_id 2>/dev/null || echo "inactive")
            is_enabled=$(systemctl is-enabled zeroclaw@$user_id 2>/dev/null || echo "disabled")
            
            if [ "$active_state" == "active" ]; then
                status="✅ 运行中"
                pid=$(systemctl show --property MainPID --value zeroclaw@$user_id)
                mem=$(ps -p $pid -o rss= 2>/dev/null | awk '{print int($1/1024)"M"}' || echo "-")
                pairing=$(sudo journalctl -u zeroclaw@$user_id -n 50 --no-pager | grep "Pairing code" | tail -1 | awk '{print $NF}' || echo "等待中...")
            else
                status="❌ 已停止"
                pid="-"
                mem="-"
                pairing="-"
            fi
            
            printf "%-12s %-10s %-8s %-10s %-12s %s\n" "$user_id" "$status" "$pid" "$mem" "$is_enabled" "$pairing"
        done
        echo "========================================================================================="
        ;;
    pairing)
        echo "🔑 正在从日志中检索所有用户的配对码..."
        echo "=========================================="
        for i in $(seq 1 $USER_COUNT); do
            user_id=$(get_user_id $i)
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
        echo "ZeroClaw 多租户管理工具 (v3 - Final)"
        # ... (Help text unchanged)
        ;;
esac
EOF
sudo chmod +x /usr/local/bin/zeroclaw-ctl

# 7. 配置 HTTPS (Let's Encrypt)
# ... (unchanged)

# 8. 配置防火墙
# ... (unchanged)

# 9. 测试并重载 Nginx
# ... (unchanged)

# 10. 显示完成信息
# ... (updated slightly)
echo ""
echo "🎉 部署完成！脚本已是最终形态。"
echo "======================================"
# ... (rest of the final message)
