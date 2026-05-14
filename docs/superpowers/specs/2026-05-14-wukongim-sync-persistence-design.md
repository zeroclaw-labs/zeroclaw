# 设计文档：WuKongIM 消息持久化与同步

为 WuKongIM 通道实现消息持久化和历史同步功能，确保在服务重启和网络重连时消息的连续性。

## 1. 需求目标

- **持久化**：记住每个频道最后接收到的消息序列号 (`last_msg_seq`) 以及全局同步版本号 (`version`)。
- **启动/重连同步**：当服务启动或重连时，通过 HTTP 同步获取缺失的消息（每个会话最多 50 条）。
- **Dawn 集成**：使用专用的 `dawn_url` 和 `dawn_token` 进行 HTTP API 调用。
- **鉴权**：HTTP 请求必须包含 `X-Assistant-Token: {dawn_token}` 请求头。
- **未读管理**：处理完历史消息后，清理服务器上的未读计数。

## 2. 配置更新 (`config.toml`)

更新 `WuKongIMConfig` 以包含：
- `dawn_url`: 用于同步和清理未读 API 的基础 URL。
- `dawn_token`: 用于 Assistant API 的鉴权令牌。

## 3. 存储模式 (Memory)

使用 `Memory` trait (SQLite 后端)：
- **全局同步版本**：`wukongim:sync:max_version` (值: `i64` 纳秒级时间戳)。
- **各频道序列号**：`wukongim:channel_seq:{channel_id}:{channel_type}` (值: `u32` 消息序列号)。

## 4. 技术设计

### 4.1 协议扩展 (`protocol.rs`)
- 为 `POST /conversation/sync` 添加 `SyncRequest` 和 `SyncResponse` 结构。
- 为 `POST /conversations/clearUnread` 添加 `ClearUnreadRequest` 结构。

### 4.2 详细同步流程 (Detailed Flow)

为确保状态一致性且 Agent 回复通道畅通，采用以下严格顺序：

1.  **HTTP 抓取 (Fetch)**：在 WebSocket 连接建立前，通过 `/conversation/sync` 获取增量消息列表，暂存在内存中。
2.  **WS 连接 (Connect)**：建立 WebSocket 连接并完成握手。此时 `ws_sink` 已就绪，Agent 的回复功能变为可用。
3.  **补丁处理 (Patch Process)**：按时间正序遍历步骤 1 抓取的消息，调用 `process_inbound_message`：
    *   发送至 Agent。
    *   **处理成功后**，更新 `Memory` 中的各频道序列号和全局版本号。
4.  **实时监听 (Live Loop)**：进入 `tokio::select!` 循环，监听 WebSocket 推送。

### 4.3 安全性与边缘情况 (Safety & Edge Cases)

*   **消息暂存 (Buffering)**：在步骤 3 处理历史补丁期间，服务器推送的实时消息会暂存在操作系统的 TCP 接收缓冲区中。一旦进入步骤 4，程序会立即读取并处理这些堆积的消息。
*   **幂等性检查 (Idempotency)**：为防止同步到的历史消息与缓冲区中的实时消息重复，处理函数在执行前会预检 `message_seq`。若收到的序列号 $\le$ 数据库记录的位点，则直接丢弃。
*   **处理优先 (Process-then-Commit)**：位点更新始终在 `tx.send()` 成功后执行。若处理中途崩溃，重启后会重新触发同步流程，确保消息“至少交付一次”。
*   **交付保证**：采用“先处理、后提交”的策略，确保 Agent 对每条消息的操作（包括回复）成功后才推进位点。

## 5. 实施步骤

1. **Schema 更新**：修改 `crates/zeroclaw-config/src/schema.rs` 添加新配置项。
2. **协议定义**：在 `crates/zeroclaw-channel-wukongim/src/connection/protocol.rs` 中添加结构体。
3. **注入 Memory**：更新 `crates/zeroclaw-channels/src/orchestrator/mod.rs` 将 Memory 注入 WuKongIM 通道。
4. **核心逻辑**：在 `crates/zeroclaw-channel-wukongim/src/channel.rs` 中实现 `sync_history` 和生命周期集成。
5. **测试**：添加单元测试验证负载构建和版本号更新逻辑。
