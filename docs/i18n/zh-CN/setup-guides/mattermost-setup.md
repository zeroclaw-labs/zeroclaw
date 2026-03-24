# Mattermost 集成指南

ZeroClaw 通过 REST API v4 原生支持与 Mattermost 集成。这种集成非常适合需要自主可控通信的自托管、私有或隔离网络环境。

## 前置条件

1.  **Mattermost 服务器**：运行中的 Mattermost 实例（自托管或云托管）。
2.  **机器人账户**：
    - 前往 **主菜单 > 集成 > 机器人账户**。
    - 点击 **添加机器人账户**。
    - 设置用户名（例如 `zeroclaw-bot`）。
    - 启用 **post:all** 和 **channel:read** 权限（或适当的作用域）。
    - 保存 **访问令牌**。
3.  **频道 ID**：
    - 打开你希望机器人监听的 Mattermost 频道。
    - 点击频道标题，选择 **查看信息**。
    - 复制 **ID**（例如 `7j8k9l...`）。

## 配置

将以下内容添加到你的 `config.toml` 的 `[channels_config]` 部分下：

```toml
[channels_config.mattermost]
url = \"https://mm.your-domain.com\"
bot_token = \"your-bot-access-token\"
channel_id = \"your-channel-id\"
allowed_users = [\"user-id-1\", \"user-id-2\"]
thread_replies = true
mention_only = true
```

### 配置字段

| 字段 | 描述 |
|---|---|
| `url` | 你的 Mattermost 服务器的基础 URL。 |
| `bot_token` | 机器人账户的个人访问令牌。 |
| `channel_id` | （可选）要监听的频道 ID。`listen` 模式下必填。 |
| `allowed_users` | （可选）允许与机器人交互的 Mattermost 用户 ID 列表。使用 `[\"*\"]` 允许所有用户。 |
| `thread_replies` | （可选）是否在话题中回复顶层用户消息。默认：`true`。现有话题中的回复始终保持在话题内。 |
| `mention_only` | （可选）当为 `true` 时，仅处理显式@机器人用户名的消息（例如 `@zeroclaw-bot`）。默认：`false`。 |

## 话题对话

ZeroClaw 在两种模式下都支持 Mattermost 话题：
- 如果用户在现有话题中发送消息，ZeroClaw 始终在同一个话题中回复。
- 如果 `thread_replies = true`（默认），顶层消息会通过创建话题来回复。
- 如果 `thread_replies = false`，顶层消息会在频道根层级回复。

## 仅@模式

当 `mention_only = true` 时，ZeroClaw 在 `allowed_users` 授权后会应用额外的过滤：

- 没有显式@机器人的消息会被忽略。
- 包含 `@bot_username` 的消息会被处理。
- `@bot_username` 标记会在发送内容给模型之前被移除。

这种模式在繁忙的共享频道中很有用，可以减少不必要的模型调用。

## 安全说明

Mattermost 集成专为**自主可控通信**设计。通过托管你自己的 Mattermost 服务器，你的代理的通信历史完全保留在你自己的基础设施中，避免第三方云服务日志记录。
