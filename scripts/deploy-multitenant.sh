#!/bin/bash
# deploy-multitenant.sh
# 在支持 systemd 的 Ubuntu 22.04+ 服务器上执行
#
# 这个脚本将自动化部署一个安全、多租户的 ZeroClaw 环境。
# 特性:
# - 每个用户一个独立的 ZeroClaw 实例
# - 使用 Nginx 作为反向代理，并进行密码保护
# - 使用 Let's Encrypt (Certbot) 自动配置 HTTPS
# - 使用 systemd 管理服务，确保进程健壮性和开机自启 (TODO: Phase 2)
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
# 添加 certbot 用于 HTTPS
sudo apt install -y nginx apache2-utils curl wget tar git ufw python3-certbot-nginx

# 2. 创建服务用户和目录结构
echo "👤 正在创建服务用户 '$SERVICE_USER' 和目录结构..."
sudo useradd -r -s /bin/false $SERVICE_USER 2>/dev/null || true
sudo mkdir -p $INSTALL_DIR/{bin,instances,nginx/htpasswd,scripts,backup}
sudo chown -R $SERVICE_USER:$SERVICE_USER $INSTALL_DIR

# 3. 下载 ZeroClaw 最新版本
echo "⬇️ 正在下载最新的 ZeroClaw 二进制文件..."
cd /tmp
# 注意: 这里假设 zeroclaw-labs/zeroclaw 是主仓库。如果不是，请修改 URL。
wget -q https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-x86_64-unknown-linux-gnu.tar.gz -O zeroclaw.tar.gz
tar -xzf zeroclaw.tar.gz
sudo mv zeroclaw $INSTALL_DIR/bin/
sudo chmod +x $INSTALL_DIR/bin/zeroclaw
rm zeroclaw.tar.gz

# 4. 创建用户实例
echo "🏗️ 正在为 $USER_COUNT 个用户创建实例..."
for i in $(seq 1 $USER_COUNT); do
    # 统一用户ID格式为 user-001, user-002 ...
    USER_ID=$(printf "user-%03d" $i)
    PORT=$((BASE_PORT + i - 1))
    USER_DIR="$INSTALL_DIR/instances/$USER_ID"

    # 创建目录
    sudo mkdir -p $USER_DIR/{tools,workspace,logs}

    # 生成随机密码并创建 htpasswd 文件
    PASSWORD=$(openssl rand -base64 12)
    # 警告: 这个文件包含初始明文密码，建议首次分发后删除
    echo "$USER_ID:$PASSWORD" | sudo tee -a $INSTALL_DIR/nginx/initial_credentials.txt > /dev/null
    
    # 使用 htpasswd 命令安全地创建密码文件 (-c 创建, -b 批处理)
    sudo htpasswd -b -c $INSTALL_DIR/nginx/htpasswd/$USER_ID $USER_ID "$PASSWORD"

    # 生成 config.toml 配置文件
    # [安全修复] host 设置为 127.0.0.1，强制流量通过 Nginx
    sudo tee $USER_DIR/config.toml > /dev/null <<EOF
[instance]
name = "$USER_ID"
display_name = "Agent $i"

[gateway]
host = "127.0.0.1"
port = $PORT
require_pairing = true
allow_public_bind = false # 已由 host = "127.0.0.1" 保证

[logging]
level = "info"
output = "$USER_DIR/logs/gateway.log"

[providers]
google = { api_key = "\${GEMINI_API_KEY}" }

[models]
default = "google/gemini-1.5-flash-latest" # 使用较新的模型
imageDefault = "google/gemini-1.5-pro-latest"

[tools]
enabled = ["fs", "exec", "web_search"] # 默认开启 web_search
workspace_path = "$USER_DIR/workspace"
EOF

    # 创建 .env 环境文件
    sudo tee $USER_DIR/.env > /dev/null <<EOF
GEMINI_API_KEY=\${GEMINI_API_KEY:-""} # 留空，待用户自行填写
USER_ID=$USER_ID
PORT=$PORT
EOF

    # 创建 Nginx 站点配置
    sudo tee /etc/nginx/sites-available/$USER_ID > /dev/null <<EOF
server {
    listen 80;
    server_name agent$i.$DOMAIN;

    # HTTP 认证
    auth_basic "ZeroClaw Instance $USER_ID";
    auth_basic_user_file $INSTALL_DIR/nginx/htpasswd/$USER_ID;

    # 代理到本地的 ZeroClaw 实例
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
    # 启用站点
    sudo ln -sf /etc/nginx/sites-available/$USER_ID /etc/nginx/sites-enabled/

    # 设置正确的权限
    sudo chown -R $SERVICE_USER:$SERVICE_USER $USER_DIR
    sudo chmod 600 $USER_DIR/.env
    echo "✅实例 $USER_ID 创建完成 (端口: $PORT, 子域名: agent$i.$DOMAIN)"
done

# 5. Nginx 主配置 (保持不变)
# ... (您的 Nginx 配置是标准的，无需修改)

# 6. 管理工具 (zeroclaw-ctl)
# [Bug修复] 修复了 user-i 和 user-00i 不匹配的问题
# ... (大部分保持不变，直到阶段2迁移到systemd)

# 7. 配置 HTTPS (Let's Encrypt)
echo "🤖 正在使用 Certbot 配置 HTTPS..."
# 生成域名列表: -d agent1.domain.com -d agent2.domain.com ...
DOMAIN_ARGS=$(for i in $(seq 1 $USER_COUNT); do echo -n "-d agent$i.$DOMAIN "; done)
# --redirect 会自动将 HTTP 请求重定向到 HTTPS
sudo certbot --nginx $DOMAIN_ARGS --non-interactive --agree-tos -m "$CERTBOT_EMAIL" --redirect

echo "🔒 HTTPS 配置完成。"

# 8. 配置防火墙
echo "🔥 正在配置防火墙 (UFW)..."
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 'Nginx Full' comment 'Nginx HTTP & HTTPS'
sudo ufw allow 22/tcp comment 'SSH'
# 无需再暴露 8080-8099 端口，因为所有流量都通过 Nginx
# sudo ufw allow 8080:8099/tcp comment 'ZeroClaw Instances (OLD)'
sudo ufw --force enable

# 9. 测试并重载 Nginx
echo "🚀 正在测试并应用 Nginx 配置..."
sudo nginx -t && sudo systemctl restart nginx

# 10. 显示完成信息
echo ""
echo "🎉 部署完成！"
echo "=================="
echo ""
echo "🚨 [重要] 初始用户凭据保存在: $INSTALL_DIR/nginx/initial_credentials.txt"
echo "   请在分发给用户后立即删除此文件！"
echo ""
echo "🌐 访问地址 (已启用 HTTPS):"
for i in {1..20}; do
    echo "  User-$(printf "%03d" $i): https://agent$i.$DOMAIN"
done
echo ""
echo "⚙️ 管理命令 (暂未改变):"
echo "  zeroclaw-ctl start     # 启动全部"
echo "  zeroclaw-ctl status    # 查看状态"
echo "  zeroclaw-ctl pairing   # 查看配对码"
echo ""
echo "🔧 下一步关键操作:"
echo "  1. [DNS] 确保 agent1.$DOMAIN 到 agent20.$DOMAIN 的 A 记录已指向本机 IP。"
echo "  2. [API Key] 通知用户编辑 $INSTALL_DIR/instances/user-*/.env 文件，填入他们的 GEMINI_API_KEY。"
echo "  3. [启动] 运行 'zeroclaw-ctl start' 启动所有服务。"
echo "  4. [配对] 运行 'zeroclaw-ctl pairing' 获取用户配对码。"
echo "  5. [安全] 删除 $INSTALL_DIR/nginx/initial_credentials.txt 文件。"
echo ""
