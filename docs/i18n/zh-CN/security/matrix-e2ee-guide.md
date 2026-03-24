# Matrix 端到端加密指南

本指南介绍如何在 Matrix 房间（包括端到端加密 (E2EE) 房间）中可靠运行 ZeroClaw。

它重点关注用户报告的常见故障模式：

> “Matrix 配置正确，检查通过，但机器人不回复。”

## 0. 快速常见问题（#499 类症状）

如果 Matrix 显示已连接但没有回复，请首先验证这些项：

1. 发送者被 `allowed_users` 允许（测试时使用：`[\"*\"]`）。
2. 机器人账户已加入正确的目标房间。
3. 令牌属于同一个机器人账户（通过 `whoami` 检查）。
4. 加密房间有可用的设备身份（`device_id`）和密钥共享。
5. 配置更改后已重启守护进程。

---

## 1. 前置条件

在测试消息流之前，请确保以下所有条件都已满足：

1. 机器人账户已加入目标房间。
2. 访问令牌属于同一个机器人账户。
3. `room_id` 正确：
   - 首选：标准房间 ID（`!room:server`）
   - 支持：房间别名（`#alias:server`），ZeroClaw 会解析它
4. `allowed_users` 允许发送者（开放测试时使用 `[\"*\"]`）。
5. 对于 E2EE 房间，机器人设备已收到房间的加密密钥。

---

## 2. 配置

使用 `~/.zeroclaw/config.toml`：

```toml
[channels_config.matrix]
homeserver = \"https://matrix.example.com\"
access_token = \"syt_your_token\"

# E2EE 稳定性可选但推荐：
user_id = \"@zeroclaw:matrix.example.com\"
device_id = \"DEVICEID123\"

# 房间 ID 或别名
room_id = \"!xtHhdHIIVEZbDPvTvZ:matrix.example.com\"
# room_id = \"#ops:matrix.example.com\"

# 初始验证期间使用 [\"*\"]，然后收紧
allowed_users = [\"*\"]
```

### 关于 `user_id` 和 `device_id`

- ZeroClaw 尝试从 Matrix `/_matrix/client/v3/account/whoami` 读取身份信息。
- 如果 `whoami` 不返回 `device_id`，请手动设置 `device_id`。
- 这些提示对于 E2EE 会话恢复尤为重要。

---

## 3. 快速验证流程

1. 运行渠道设置和守护进程：

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

2. 在配置的 Matrix 房间中发送纯文本消息。

3. 确认 ZeroClaw 日志包含 Matrix 监听器启动信息，没有重复的同步/认证错误。

4. 在加密房间中，验证机器人可以读取并回复允许用户的加密消息。

---

## 4. “无响应”故障排除

按顺序使用此检查清单。

### A. 房间和成员资格

- 确保机器人账户已加入房间。
- 如果使用别名（`#...`），验证它解析为预期的标准房间。

### B. 发送者白名单

- 如果 `allowed_users = []`，所有入站消息都会被拒绝。
- 诊断时，临时设置 `allowed_users = [\"*\"]`。

### C. 令牌和身份

- 使用以下命令验证令牌：

```bash
curl -sS -H \"Authorization: Bearer $MATRIX_TOKEN\" \
  \"https://matrix.example.com/_matrix/client/v3/account/whoami\"
```

- 检查返回的 `user_id` 与机器人账户匹配。
- 如果缺少 `device_id`，手动设置 `channels_config.matrix.device_id`。

### D. E2EE 特定检查

- 机器人设备必须从受信任设备接收房间密钥。
- 如果密钥未共享到此设备，加密事件无法解密。
- 在你的 Matrix 客户端/管理工作流中验证设备信任和密钥共享。
- 如果日志显示 `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found`，说明此设备尚未启用密钥备份恢复。此警告通常对实时消息流非致命，但你仍应完成密钥备份/恢复设置。
- 如果接收者看到机器人消息为“未验证”，从受信任的 Matrix 会话验证/签名机器人设备，并在重启期间保持 `channels_config.matrix.device_id` 稳定。

### E. 消息格式（Markdown）

- ZeroClaw 将 Matrix 文本回复作为支持 markdown 的 `m.room.message` 文本内容发送。
- 支持 `formatted_body` 的 Matrix 客户端应渲染强调、列表和代码块。
- 如果格式显示为纯文本，首先检查客户端能力，然后确认 ZeroClaw 运行的构建包含启用 markdown 的 Matrix 输出。

### F. 全新启动测试

更新配置后，重启守护进程并发送新消息（不只是旧时间线历史）。

---

## 5. 操作说明

- 不要将 Matrix 令牌暴露在日志和截图中。
- 从宽松的 `allowed_users` 开始，然后收紧为明确的用户 ID。
- 生产环境中首选标准房间 ID 以避免别名漂移。

---

## 6. 相关文档

- [渠道参考](../reference/api/channels-reference.zh-CN.md)
- [操作日志关键词附录](../reference/api/channels-reference.zh-CN.md#7-操作附录日志关键词矩阵)
- [网络部署](../ops/network-deployment.zh-CN.md)
- [不可知安全](./agnostic-security.zh-CN.md)
- [评审者手册](../contributing/reviewer-playbook.zh-CN.md)
