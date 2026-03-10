# Node-Control WebSocket 与 nodes 工具改造方案

本文档草拟 ZeroClaw 的 **node-control** 改造方案：Gateway 提供 WebSocket 服务供 node（如 Android）连接，Agent 通过 **nodes** 工具向已连接 node 下发指令。设计目标与现有 `[gateway.node_control]` 及 `POST /api/node-control` 对齐，并在此基础上扩展。

---

## 1. 目标与范围

### 1.1 目标

- **Gateway 提供 WebSocket 服务**：node 客户端（如 OpenClaw Android App）主动连接 Gateway，维持长连接。
- **已连接 node 注册与可见**：Gateway 维护“已连接 node”列表（含 node_id、能力、状态），供 Agent 与 API 使用。
- **Agent 的 nodes 工具**：在工具集中提供 `nodes` 工具，支持列出 node、查看描述、对指定 node 执行 **invoke**（结构化命令）或 **run**（原始命令），结果返回给 Agent。
- **与现有 node-control 兼容**：`POST /api/node-control` 的 `node.list` / `node.describe` / `node.invoke` 改为基于“已连接 node 注册表”实现，行为与 scaffold 一致或更完整。

### 1.2 非目标（本阶段）

- 不实现具体 node 端（如 Android App）；仅定义 Gateway 侧协议与行为。
- 不实现 mDNS 发现、Tailscale 等发现/组网细节；node 通过配置的 Gateway 地址连接即可。
- 配对/审批流程可先做“连接即通过”的简化版，完整审批流程（pending → approve）可后续迭代。

---

## 2. 架构概览

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  Gateway                                                                     │
│  - WebSocket 服务: GET /ws/node (node 连接入口)                               │
│  - 已连接 node 注册表: NodeRegistry                                          │
│  - POST /api/node-control: node.list / node.describe / node.invoke 走注册表   │
│  - Agent 工具集包含 nodes 工具 (仅当 node_control.enabled 时)                 │
└───────────────┬─────────────────────────────┬───────────────────────────────┘
                │                             │
    Node 连接 (WebSocket)              Agent 调用 nodes 工具
                │                             │
                ▼                             ▼
┌───────────────────────────────┐   ┌────────────────────────────────────────┐
│  Node 客户端 (如 Android)     │   │  nodes 工具                             │
│  - 连接 ws://host:port/ws/node │   │  - nodes_list()  → 从 NodeRegistry 读   │
│  - 注册 node_id / capabilities │   │  - nodes_describe(node_id)              │
│  - 接收 invoke/run，执行后回传  │◄──│  - nodes_invoke(node_id, cmd, params)   │
└───────────────────────────────┘   │  - nodes_run(node_id, raw_command)       │
                                    └────────────────────────────────────────┘
```

- **左线**：node 连上 Gateway 的 WebSocket，注册自身；Gateway 将连接与 node_id 存入 NodeRegistry。
- **右线**：Agent 通过 nodes 工具或 HTTP `POST /api/node-control` 发起 list/describe/invoke/run，Gateway 从 NodeRegistry 解析目标 node，通过对应 WebSocket 下发指令并收集结果。

---

## 3. 模块与职责

### 3.1 NodeRegistry（已连接 node 注册表）

- **职责**：保存当前所有“通过 WebSocket 连接并已完成注册”的 node；支持按 node_id 查询、下发指令、移除断开连接。
- **建议位置**：`src/gateway/node_registry.rs`（或 `src/gateway/nodes/` 子模块）。
- **接口（供 Gateway 与 nodes 工具使用）**：
  - `list() -> Vec<NodeInfo>`：当前已连接 node 列表（node_id、状态、capabilities 等）。
  - `describe(node_id: &str) -> Option<NodeDescription>`：单个 node 描述。
  - `invoke(node_id: &str, capability: &str, arguments: Value) -> Result<InvokeResult>`：向 node 发送结构化 invoke，带超时。
  - `run(node_id: &str, raw_command: &str) -> Result<RunResult>`：向 node 发送原始命令（如 shell 片段），带超时。
- **内部结构**：可维护 `Map<node_id, NodeSession>`；`NodeSession` 至少包含：
  - 该 node 的 WebSocket 发送端（用于下发请求、接收响应）。
  - 注册时上报的 node_id、capabilities、可选元数据。
  - 连接状态（connected / disconnected 等）。
- **并发**：需线程安全（如 `RwLock<HashMap<...>>` 或 `DashMap`），以便在 WebSocket 回调和 Agent 工具调用间共享。

为便于 **tools** 与 **gateway** 解耦，建议在 `src/tools/` 中定义 **NodeRegistry 的 trait**，Gateway 内实现该 trait 的具体类型并注入到 nodes 工具。

### 3.2 Node WebSocket 服务

- **路由**：新增 `GET /ws/node`（与现有 `GET /ws/chat` 区分：chat 面向前端用户，node 面向 node 设备）。
- **协议（建议）**：
  - **Node → Gateway**：
    - 连接建立后发送 **register**：`{"type":"register","node_id":"...","capabilities":[...],"meta":{...}}`。
    - 对 invoke/run 的响应：`{"type":"invoke_result","request_id":"...","ok":true,"output":"..."}` 或 `{"type":"run_result",...}`。
  - **Gateway → Node**：
    - **invoke**：`{"type":"invoke","request_id":"...","capability":"...","arguments":{...}}`。
    - **run**：`{"type":"run","request_id":"...","command":"..."}`。
- **认证**：可选。若 `[gateway.node_control]` 配置了 `auth_token`，node 可在连接时通过 query 或首帧携带 token；未配置则仅依赖“连接即注册”（或后续扩展审批流）。
- **生命周期**：连接建立 → 收到合法 register → 写入 NodeRegistry；连接断开或超时未注册 → 从 NodeRegistry 移除。

实现位置：`src/gateway/ws_node.rs`（或 `src/gateway/nodes/ws.rs`），在 `run_gateway` 中挂载路由。

### 3.3 nodes 工具（Agent 侧）

- **名称**：`nodes`（与 OpenClaw 语义一致）。
- **建议位置**：`src/tools/nodes.rs`。
- **依赖**：仅依赖 `NodeRegistry` trait（定义在 tools 或共享层），不直接依赖 Gateway 或 WebSocket。
- **行为**：
  - 工具对外一个入口 `execute(args)`，根据参数中的 **action** 分发：
    - `list`：返回当前已连接 node 列表（来自 `NodeRegistry::list()`）。
    - `describe`：`node_id` 必填，返回 `NodeRegistry::describe(node_id)`。
    - `invoke`：`node_id`、`capability`、`arguments` 必填，调用 `NodeRegistry::invoke(...)`，返回结果摘要或错误信息。
    - `run`：`node_id`、`command` 必填，调用 `NodeRegistry::run(...)`，返回执行输出或错误。
- **参数 schema**：JSON Schema 中为 LLM 明确列出 action、node_id、capability、arguments、command 等，便于正确调用。
- **可见性**：仅当 `[gateway.node_control].enabled = true` 且在 **Gateway 进程** 中构建工具集时，才将 `nodes` 加入 `tools_registry_exec`；CLI/channel 等非 Gateway 场景可不注入（或通过配置控制），避免无 node 环境误用。

### 3.4 现有 POST /api/node-control 的改造

- **node.list**：不再仅从配置的 `allowed_node_ids` 返回静态列表；改为从 **NodeRegistry::list()** 取已连接 node，再与 `allowed_node_ids` 做过滤（若配置非空）。未启用 NodeRegistry 时，可回退为当前“静态 stub”行为以保持兼容。
- **node.describe**：从 **NodeRegistry::describe(node_id)** 取；若无则 404 或与现有 stub 一致。
- **node.invoke**：调用 **NodeRegistry::invoke(node_id, capability, arguments)**，将结果转为现有 JSON 响应格式；失败时返回 5xx 或 4xx 及错误信息。

这样 HTTP API 与 nodes 工具共用同一套 NodeRegistry，行为一致。

### 3.5 配置与 Gateway 启动

- **现有**：`[gateway.node_control]` 已有 `enabled`、`auth_token`、`allowed_node_ids`。保留不变。
- **可选扩展**（后续可加）：
  - `ws_path`：node WebSocket 路径，默认 `"/ws/node"`。
  - `require_approval`：是否要求 node 连接后经“审批”才加入注册表（本阶段可忽略，连接即注册）。
- **启动顺序**：
  1. 若 `node_control.enabled`，创建 `NodeRegistry` 实例（如 `Arc<ConnectedNodeRegistry>`）。
  2. 注册 `GET /ws/node`，传入 `AppState`（或包含 NodeRegistry 的 state）。
  3. 构建 `tools_registry_exec`：先 `all_tools_with_runtime(...)`，再若 `node_control.enabled` 则 `push(NodesTool::new(Arc::clone(&node_registry)))`，最后放入 `AppState`。
  4. 将 `node_registry` 存入 `AppState`（或通过 state 传给 `handle_node_control`），以便 `POST /api/node-control` 使用同一注册表。

---

## 4. 依赖与边界

- **tools 与 gateway**：
  - **tools**：定义 `NodeRegistry` trait 与 `NodesTool`；不依赖 axum、WebSocket。
  - **gateway**：实现 `NodeRegistry` 的具体类型（持有 WebSocket 会话、map 等），在 run_gateway 中创建并注入到 NodesTool 与 `handle_node_control`。
- **可选**：若希望 tools 完全无 gateway 依赖，可将 `NodeRegistry` trait 放在 `src/tools/`，gateway 通过 `impl NodeRegistry for ConnectedNodeRegistry` 在 gateway  crate 内实现。

---

## 5. 安全与策略

- **allowlist**：保留 `allowed_node_ids`。若配置非空，则仅允许 list/describe/invoke/run 针对该列表中的 node_id；NodeRegistry 中其他 node 对 API 与工具不可见（或列表过滤掉）。
- **认证**：node 连接时的 token 校验（若配置 `auth_token`）；`POST /api/node-control` 继续使用现有 pairing 与可选 `X-Node-Control-Token`。
- **超时与限流**：invoke/run 建议带超时（如 15s）；可对单 node 或全局做简单限流，防止滥用。
- **日志**：不记录敏感参数内容；可记录 node_id、action、request_id、成功/失败，便于排障与审计。

---

## 6. 实现顺序建议

1. **NodeRegistry trait + 内存实现**
   - 在 `src/tools/` 定义 `NodeRegistry` trait 与 `NodesTool`；在 gateway 中实现一个仅内存的 `ConnectedNodeRegistry`（先不接 WebSocket，仅 stub invoke/run 返回“未实现”），用于把“工具 + HTTP API 走注册表”的链路打通。
2. **GET /ws/node 与协议**
   - 实现 node 端 WebSocket 连接与 register 报文解析；连接后写入 `ConnectedNodeRegistry`，断开时移除。
3. **invoke/run 与 request_id 对应**
   - 在 NodeSession 中维护 request_id → 回调/oneshot，Gateway 发 invoke/run 后等待 node 回传对应 request_id 的结果，再返回给调用方（工具或 HTTP）。
4. **POST /api/node-control 切换为注册表**
   - node.list / node.describe / node.invoke 全部改为从 NodeRegistry 读写；与 nodes 工具行为对齐。
5. **策略与文档**
   - 完善 allowed_node_ids、超时、日志；更新 `docs/config-reference.md` 与 `docs/commands-reference.md`（若有 CLI 扩展）。

---

## 7. 与 OpenClaw 的对应关系

| OpenClaw 概念           | 本方案对应 |
|-------------------------|------------|
| Gateway WebSocket 服务  | `GET /ws/node`，Gateway 提供 |
| Android 等设备作为 node | 第三方客户端连接 `/ws/node` 并发送 register |
| Agent 的 nodes 工具     | `NodesTool`，action: list / describe / invoke / run |
| openclaw nodes invoke   | `nodes` 工具 action=invoke 或 `POST /api/node-control` method=node.invoke |
| openclaw nodes run      | `nodes` 工具 action=run（可选扩展 HTTP API） |
| 配对/审批               | 本阶段“连接即注册”；后续可加 pending → approve |

---

## 8. 文档与引用

- 现有 node-control 配置：`docs/config-reference.md` — `[gateway.node_control]`。
- 现有 scaffold 行为：`src/gateway/mod.rs` — `handle_node_control`、`NodeControlRequest`。
- 工具扩展约定：`AGENTS.md` §7.3 Adding a Tool。
- 可参考的 WebSocket 与状态管理：`src/gateway/ws.rs`（chat）、`AppState` 结构。

本方案为草拟稿，后续实现时可按需微调路径命名、报文格式和配置项，并保持与现有 node-control 的兼容性。

---

## 9. 排障复盘：`/response` 看不到 `nodes` 工具

这部分记录一个高频坑位：`nodes` 工具已注册，但通过 `POST /response` 对话时模型仍回答“没有 nodes 工具”。

### 9.1 现象

- `GET /api/tools` 返回中可以看到 `nodes`（说明 Gateway 工具注册成功）。
- `POST /response` 中，模型仅报告基础工具（如 shell/file/memory），不识别 `nodes`。

### 9.2 根因

根因不在 `nodes` 工具实现本身，而在 **tool specs 传递条件**：

- 在 `run_tool_call_loop` 中，历史逻辑只在 `use_native_tools == true` 时才把 `tools` 传给 `provider.chat(...)`。
- 对 `minimax-cn` 这类 `supports_native_tools == false` 的 provider，`request.tools` 被置为 `None`。
- provider 侧的 prompt-guided fallback 依赖 `request.tools` 注入工具说明；当其为 `None` 时，模型拿不到完整工具清单，`nodes` 不可见。

换句话说：工具已注册，但没有被完整“告知模型”。

### 9.3 触发条件

同时满足以下条件时容易触发：

1. Gateway 已启用 node-control，`nodes` 已注入工具表；
2. 当前 provider 不支持 native tool calling（如 MiniMax 兼容链路）；
3. tool loop 只在 native 模式传递 `request.tools`。

### 9.4 修复方案

将 tool specs 传递条件从“仅 native 模式”改为“只要有工具就传”：

- 旧逻辑：`if use_native_tools { Some(tool_specs) } else { None }`
- 新逻辑：`if !tool_specs.is_empty() { Some(tool_specs) } else { None }`

这样可保证：

- native provider 仍可走原生工具调用；
- non-native provider 也能收到完整工具说明并走 prompt-guided 工具调用；
- `nodes` 在 `/response` 场景中可被模型识别。

### 9.5 配置项与代码行为边界

- **配置能控制**：`[gateway.node_control].enabled`（是否注册 `nodes` 到 Gateway 工具集）。
- **配置不能直接控制**：provider 在一次请求里是否收到完整 tools payload。
- `minimax-cn` 的 `native_tool_calling = false` 是 provider 构造策略，不是单独配置开关。

因此该问题的关键修复点是调用链代码，不是增加新配置。
