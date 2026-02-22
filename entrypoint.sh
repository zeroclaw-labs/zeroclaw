#!/bin/sh
set -e

echo "🚀 Starting zeroclaw..."

# 启动 zeroclaw（后台）
zeroclaw gateway --config-dir /zeroclaw-data/.zeroclaw &

echo "⏳ Waiting for zeroclaw to start..."
sleep 8

# 自动注册 Telegram webhook
if [ -n "$TELEGRAM_BOT_TOKEN" ] && [ -n "$ZEROCLAW_WEBHOOK_BASE" ]; then
  echo "🔗 Registering Telegram webhook..."

  curl -s \
    "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/setWebhook?url=${ZEROCLAW_WEBHOOK_BASE}/webhook"

  echo ""
  echo "✅ Telegram webhook registered"
else
  echo "⚠️ TELEGRAM_BOT_TOKEN not set, skipping webhook"
fi

wait
