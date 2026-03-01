#!/bin/bash
# deploy-20users-zeroclaw.sh
# 在 OVH 服务器上执行
set -e

USER_COUNT=20
BASE_PORT=8080
DOMAIN=${DOMAIN:-"yourdomain.com"} # 修改为你的域名
INSTALL_DIR="/opt/zeroclaw"
SERVICE_USER="zeroclaw"

echo "🚀 ZeroClaw 20用户多租户部署"
echo "=============================="
echo "配置: 4核8GB/75G"
echo "用户数: $USER_COUNT"
echo "域名: $DOMAIN"
echo ""

# 1. 系统准备
echo "📦 系统更新..."
sudo apt update && sudo apt upgrade -y
sudo apt install -y nginx apache2-utils curl wget tar git ufw

# 2. 创建用户
sudo useradd -r -s /bin/false $SERVICE_USER 2>/dev/null || true
sudo mkdir -p $INSTALL_DIR/{bin,instances,nginx/htpasswd,scripts,backup}
sudo chown -R $SERVICE_USER:$SERVICE_USER $INSTALL_DIR

# 3. 下载 ZeroClaw
echo "⬇️ 下载 ZeroClaw..."
cd /tmp
wget -q https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-x86_64-unknown-linux-gnu.tar.gz
tar -xzf zeroclaw*.tar.gz
sudo mv zeroclaw $INSTALL_DIR/bin/
sudo chmod +x $INSTALL_DIR/bin/zeroclaw

# 4. 创建 20 个用户实例
echo "🏗️ 创建 $USER_COUNT 个用户实例..."
for i in $(seq -w 1 $USER_COUNT); do
    USER_ID="user-$i"
    PORT=$((BASE_PORT + i - 1))
    USER_DIR="$INSTALL_DIR/instances/$USER_ID"

    # 创建目录
    sudo mkdir -p $USER_DIR/{tools,workspace,logs}

    # 生成随机密码（保存到文件）
    PASSWORD=$(openssl rand -base64 12)
    echo "$USER_ID:$PASSWORD" | sudo tee -a $INSTALL_DIR/nginx/credentials.txt > /dev/null

    # 创建 Nginx 密码文件
    echo "$USER_ID:$(openssl passwd -apr1 $PASSWORD)" | sudo tee $INSTALL_DIR/nginx/htpasswd/$USER_ID > /dev/null

    # 生成配置（默认用 Gemini，可后续修改）
    sudo tee $USER_DIR/config.toml > /dev/null <<EOF
[instance]
name = "$USER_ID"
display_name = "Agent $i"

[gateway]
host = "0.0.0.0"
port = $PORT
require_pairing = true
allow_public_bind = true

[logging]
level = "info"
output = "$USER_DIR/logs/gateway.log"

[providers]
google = { api_key = "\${GEMINI_API_KEY}" }

[models]
default = "google/gemini-2.5-flash"
imageDefault = "google/gemini-3-image-preview"

[tools]
enabled = ["fs", "exec"]
workspace_path = "$USER_DIR/workspace"
EOF

    # 环境文件
    sudo tee $USER_DIR/.env > /dev/null <<EOF
GEMINI_API_KEY=\${GEMINI_API_KEY:-""}
USER_ID=$USER_ID
PORT=$PORT
EOF

    # Nginx 配置
    sudo tee /etc/nginx/sites-available/$USER_ID > /dev/null <<EOF
server {
    listen 80;
    server_name agent$i.$DOMAIN;

    auth_basic "ZeroClaw $USER_ID";
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

    # 权限
    sudo chown -R $SERVICE_USER:$SERVICE_USER $USER_DIR
    sudo chmod 600 $USER_DIR/.env
    echo "✅ User-$i 创建完成 (端口: $PORT)"
done

# 5. Nginx 主配置
sudo tee /etc/nginx/nginx.conf > /dev/null <<'EOF'
user www-data;
worker_processes auto;
pid /run/nginx.pid;

events {
    worker_connections 1024;
}

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    log_format main '$remote_addr - $remote_user [$time_local] "$request" '
                      '$status $body_bytes_sent "$http_referer" '
                      '"$http_user_agent" "$http_x_forwarded_for"';
    access_log /var/log/nginx/access.log main;
    error_log /var/log/nginx/error.log;
    sendfile on;
    tcp_nopush on;
    tcp_nodelay on;
    keepalive_timeout 65;
    include /etc/nginx/conf.d/*.conf;
    include /etc/nginx/sites-enabled/*;
}
EOF

# 6. 管理工具
sudo tee /usr/local/bin/zeroclaw-ctl > /dev/null <<'EOF'
#!/bin/bash
INSTALL_DIR="/opt/zeroclaw"

CMD=$1
NUM=$2

case "$CMD" in
    start)
        if [ -z "$NUM" ]; then
            echo "🚀 启动所有 20 个实例..."
            for i in {1..20}; do
                user=$(printf "user-%03d" $i)
                dir="$INSTALL_DIR/instances/$user"
                [ ! -d "$dir" ] && continue
                port=$((8080 + i - 1))
                
                # 检查是否已运行
                if pgrep -f "zeroclaw.*$port" > /dev/null; then
                    echo " $user 已在运行"
                    continue
                fi

                cd $dir
                sudo -u zeroclaw nohup $INSTALL_DIR/bin/zeroclaw gateway \
                    --config $dir/config.toml > $dir/logs/gateway.log 2>&1 &
                echo " ✅ $user 启动完成 (端口 $port)"
                sleep 0.5
            done
        else
            user=$(printf "user-%03d" $NUM)
            dir="$INSTALL_DIR/instances/$user"
            cd $dir
            sudo -u zeroclaw nohup $INSTALL_DIR/bin/zeroclaw gateway \
                --config $dir/config.toml > $dir/logs/gateway.log 2>&1 &
            echo "✅ $user 启动完成"
        fi
        ;;
    stop)
        if [ -z "$NUM" ]; then
            echo "🛑 停止所有实例..."
            pkill -f "zeroclaw gateway" || true
        else
            user=$(printf "user-%03d" $NUM)
            pkill -f "zeroclaw.*user-$NUM" || true
            echo "🛑 $user 已停止"
        fi
        ;;
    status)
        echo "📊 ZeroClaw 20 用户状态"
        echo "================================================================"
        printf "%-12s %-6s %-8s %-8s %-10s %s\n" "用户" "端口" "状态" "PID" "内存" "配对码"
        echo "----------------------------------------------------------------"
        for i in {1..20}; do
            user=$(printf "user-%03d" $i)
            port=$((8080 + i - 1))
            dir="$INSTALL_DIR/instances/$user"
            [ ! -d "$dir" ] && continue
            
            pid=$(pgrep -f "zeroclaw.*port $port" | head -1 || echo "-")
            
            if [ "$pid" != "-" ]; then
                status="✅ 运行"
                mem=$(ps -p $pid -o rss= 2>/dev/null | awk '{print int($1/1024)"M"}' || echo "-")
                # 从日志获取配对码
                pairing=$(sudo grep "Pairing code" $dir/logs/gateway.log 2>/dev/null | tail -1 | awk '{print $3}' || echo "等待中")
            else
                status="❌ 停止"
                mem="-"
                pairing="-"
            fi
            printf "%-12s %-6s %-8s %-8s %-10s %s\n" "$user" "$port" "$status" "$pid" "$mem" "$pairing"
        done
        echo "================================================================"
        ;;
    pairing)
        echo "🔑 所有用户配对码："
        echo "=========================================="
        for i in {1..20}; do
            user=$(printf "user-%03d" $i)
            dir="$INSTALL_DIR/instances/$user"
            [ ! -f "$dir/logs/gateway.log" ] && continue
            code=$(sudo grep "Pairing code" $dir/logs/gateway.log 2>/dev/null | tail -1 | awk '{print $3}' || echo "未生成")
            echo "User-$i: $code"
        done
        ;;
    logs)
        [ -z "$NUM" ] && echo "用法: zeroclaw-ctl logs <用户号(1-20)>" && exit 1
        user=$(printf "user-%03d" $NUM)
        sudo tail -f $INSTALL_DIR/instances/$user/logs/gateway.log
        ;;
    restart)
        $0 stop $NUM
        sleep 2
        $0 start $NUM
        ;;
    password)
        [ -z "$NUM" ] && echo "用法: zeroclaw-ctl password <用户号>" && exit 1
        user=$(printf "user-%03d" $NUM)
        read -s -p "输入新密码: " newpass
        echo ""
        sudo htpasswd -b $INSTALL_DIR/nginx/htpasswd/$user $user "$newpass"
        echo "✅ 密码已更新"
        ;;
    *)
        echo "ZeroClaw 20用户管理工具"
        echo "用法: zeroclaw-ctl <命令> [参数]"
        echo ""
        echo "命令:"
        echo "  start [n]    启动所有或第n个实例 (n=1-20)"
        echo "  stop [n]     停止所有或第n个实例"
        echo "  restart [n]  重启所有或第n个实例"
        echo "  status       查看所有实例状态"
        echo "  pairing      查看所有配对码"
        echo "  logs <n>     查看第n个用户日志"
        echo "  password <n> 重置第n个用户Web密码"
        echo ""
        echo "示例:"
        echo "  zeroclaw-ctl start     # 启动全部"
        echo "  zeroclaw-ctl status    # 查看状态"
        echo "  zeroclaw-ctl logs 5    # 查看user-005日志"
        ;;
esac
EOF
sudo chmod +x /usr/local/bin/zeroclaw-ctl

# 7. 防火墙配置
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp comment 'SSH'
sudo ufw allow 80/tcp comment 'HTTP'
sudo ufw allow 8080:8099/tcp comment 'ZeroClaw 20 instances'
sudo ufw --force enable

# 8. 测试并重载 Nginx
sudo nginx -t && sudo systemctl restart nginx

# 9. 显示完成信息
echo ""
echo "🎉 部署完成！"
echo "=================="
echo ""
echo "📋 用户凭据保存在: $INSTALL_DIR/nginx/credentials.txt"
echo "   (包含 Web 登录密码)"
echo ""
echo "🌐 访问地址:"
for i in {1..20}; do
    echo "  User-$(printf "%03d" $i): http://agent$i.$DOMAIN"
done
echo ""
echo "⚙️ 管理命令:"
echo "  zeroclaw-ctl start     # 启动全部"
echo "  zeroclaw-ctl status    # 查看状态"
echo "  zeroclaw-ctl pairing   # 查看配对码"
echo ""
echo "🔧 下一步:"
echo "  1. 设置 DNS: 将 agent1.$DOMAIN ~ agent20.$DOMAIN 指向本机IP"
echo "  2. 配置 API Key: 编辑各用户的 ~/.env 文件"
echo "  3. 启动: zeroclaw-ctl start"
echo "  4. 查看配对码: zeroclaw-ctl pairing"
echo ""
