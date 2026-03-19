# Nextcloud Talk 安装指南

本指南介绍 JhedaiClaw 的原生 Nextcloud Talk 集成。

## 1. 集成功能

- 通过 `POST /nextcloud-talk` 接收传入的 Talk 机器人 webhook 事件。
- 配置密钥时验证 webhook 签名（HMAC-SHA256）。
- 通过 Nextcloud OCS API 向 Talk 房间发送机器人回复。

## 2. 配置

在 `~/.jhedaiclaw/config.toml` 中添加以下部分：

```toml
[channels_config.nextcloud_talk]
base_url = \"https://cloud.example.com\"
app_token = \"nextcloud-talk-app-token\"
webhook_secret = \"optional-webhook-secret\"
allowed_users = [\"*\"]
```

字段说明：

- `base_url`：Nextcloud 基础 URL。
- `app_token`：机器人应用令牌，用作 OCS 发送 API 的 `Authorization: Bearer <token>`。
- `webhook_secret`：用于验证 `X-Nextcloud-Talk-Signature` 的共享密钥。
- `allowed_users`：允许的 Nextcloud 参与者 ID（`[]` 拒绝所有，`\"*\"` 允许所有）。

环境变量覆盖：

- 设置 `JHEDAICLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` 时会覆盖 `webhook_secret`。

## 3. 网关端点

运行守护进程或网关并暴露 webhook 端点：

```bash
jhedaiclaw daemon
# 或
jhedaiclaw gateway --host 127.0.0.1 --port 3000
```

将你的 Nextcloud Talk 机器人 webhook URL 配置为：

- `https://<your-public-url>/nextcloud-talk`

## 4. 签名验证规则

配置 `webhook_secret` 时，JhedaiClaw 会验证：

- 请求头 `X-Nextcloud-Talk-Random`
- 请求头 `X-Nextcloud-Talk-Signature`

验证公式：

- `hex(hmac_sha256(secret, random + raw_request_body))`

如果验证失败，网关返回 `401 Unauthorized`。

## 5. 消息路由行为

- JhedaiClaw 忽略来自机器人的 webhook 事件（`actorType = bots`）。
- JhedaiClaw 忽略非消息/系统事件。
- 回复路由使用 webhook 负载中的 Talk 房间令牌。

## 6. 快速验证清单

1. 首次验证时设置 `allowed_users = [\"*\"]`。
2. 在目标 Talk 房间发送测试消息。
3. 确认 JhedaiClaw 收到消息并在同一房间回复。
4. 将 `allowed_users` 收紧为明确的参与者 ID。

## 7. 故障排除

- `404 Nextcloud Talk not configured`：缺少 `[channels_config.nextcloud_talk]` 配置。
- `401 Invalid signature`：`webhook_secret`、随机数请求头或原始体签名不匹配。
- webhook 返回 `200` 但无回复：事件被过滤（机器人/系统/非允许用户/非消息负载）。
