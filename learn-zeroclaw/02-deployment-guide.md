# ZeroClaw 部署与使用指南

> 本指南基于 `learn-zeroclaw/` 目录下的 Docker 部署环境。

---

## 1. 目录结构

```
learn-zeroclaw/
├── docker-compose.yml          # 服务编排
├── config.toml                 # ZeroClaw 网关/守护进程配置
├── .env                        # API Key（不提交到 git）
├── .gitignore
│
├── workspace/                  # Agent 的工作空间（身份 + 记忆 + 状态）
│   ├── IDENTITY.md             # Agent 身份（名字、特征）
│   ├── SOUL.md                 # 核心人格、价值观、沟通风格
│   ├── USER.md                 # 用户信息（你是谁、偏好）
│   ├── AGENTS.md               # 会话启动指令（每次对话前读什么）
│   ├── TOOLS.md                # 本地环境说明（工具备注）
│   ├── HEARTBEAT.md            # 定期任务队列（心跳检查的任务列表）
│   ├── MEMORY.md               # 长期记忆（Agent 自动维护）
│   ├── sessions/               # 会话日志
│   ├── memory/                 # 每日记忆文件 (YYYY-MM-DD.md)
│   ├── state/                  # 内部状态
│   ├── cron/                   # 定时任务配置
│   └── skills/                 # 自定义技能包
│
└── web-ui/                     # React 前端
    ├── Dockerfile
    ├── nginx.conf              # 反向代理 /api/* → zeroclaw:3000
    └── src/
        ├── App.tsx             # 入口：连接检测 → 配对 → 聊天
        ├── components/
        │   ├── PairingForm.tsx # 6 位码配对页
        │   └── Chat.tsx        # 聊天界面
        └── lib/
            └── api.ts          # API 封装：health / pair / webhook
```

---

## 2. 快速启动

### 前置条件
- Docker Desktop 已安装且正在运行
- 一个 OpenRouter API Key（或其他支持的 LLM 提供商）

### 启动

```bash
cd learn-zeroclaw

# 编辑 .env，填入你的 API Key
# API_KEY=sk-or-v1-xxxx

# 启动所有服务
docker compose up -d --build

# 查看 ZeroClaw 日志（含 pairing code）
docker logs zeroclaw-learn
```

### 访问

1. 打开浏览器：**http://localhost:8080**
2. 从日志中获取 6 位配对码：
   ```bash
   docker logs zeroclaw-learn 2>&1 | grep -A3 'one-time code'
   ```
3. 在页面输入配对码，点击 Connect
4. 开始聊天

### 停止

```bash
docker compose down
```

---

## 3. 运行模式说明

当前部署使用 `daemon` 模式，它同时启动 4 个组件：

| 组件 | 功能 | 状态 |
|------|------|------|
| **Gateway** | HTTP 网关，提供 `/health`、`/pair`、`/webhook` 端点 | 运行中 |
| **Channels** | 实时聊天平台监听（Telegram/Discord 等） | 未配置，已跳过 |
| **Scheduler** | 定时任务执行引擎 | 运行中（空闲） |
| **Heartbeat** | 读取 `HEARTBEAT.md` 定期执行任务 | 运行中（空闲） |

### 两种对话链路对比

```
┌─────────────────────────────────────────────────────────┐
│  Web UI → /webhook (Gateway)                            │
│  链路：用户消息 → LLM → 回复                              │
│  能力：纯对话 ✅  工具调用 ❌  记忆 ❌  系统提示词 ❌       │
│  原因：/webhook 用的是 simple_chat()，只做单轮 LLM 调用    │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│  Telegram/Discord → Channel Listener (Daemon)           │
│  链路：用户消息 → 系统提示词 → Memory Recall → LLM        │
│        → Tool Call Loop → Memory Save → 回复             │
│  能力：纯对话 ✅  工具调用 ✅  记忆 ✅  系统提示词 ✅       │
│  原因：Channel 走完整的 run_tool_call_loop()              │
└─────────────────────────────────────────────────────────┘
```

**要体验完整 Agent 能力（工具+记忆+身份）**，需要配置一个 Channel（如 Telegram Bot），
或者在容器内直接运行交互式 Agent：

```bash
docker exec -it zeroclaw-learn zeroclaw agent
```

---

## 4. 系统提示词体系

ZeroClaw 的系统提示词不是一个固定字符串，而是从 workspace 中多个 markdown 文件**动态组装**而成。

### 组装顺序（源码：`src/agent/prompt.rs`）

```
系统提示词 = 拼接以下内容：

1. AGENTS.md    ← 会话启动指令（每次对话第一步做什么）
2. SOUL.md      ← 核心人格（"你是谁"的定义）
3. TOOLS.md     ← 工具说明和环境备注
4. IDENTITY.md  ← 自我身份（名字、特征）
5. USER.md      ← 用户信息（你在帮谁）
6. HEARTBEAT.md ← 定期任务
7. BOOTSTRAP.md ← 首次会话引导（用完后自删）
8. MEMORY.md    ← 长期记忆（仅主会话注入）

+ Tools Schema  ← 所有注册工具的 JSON Schema（自动生成）
+ Safety Rules  ← 安全约束（自动附加）
+ DateTime      ← 当前日期时间（自动附加）
```

### 各文件职责

| 文件 | 位置 | 用途 | 谁来维护 |
|------|------|------|----------|
| **IDENTITY.md** | `workspace/IDENTITY.md` | Agent 的名字和基本特征 | 人类初始化，Agent 可自我更新 |
| **SOUL.md** | `workspace/SOUL.md` | 核心人格、沟通风格、边界规则 | 人类编写 |
| **USER.md** | `workspace/USER.md` | 用户的名字、时区、偏好、工作上下文 | 人类编写，Agent 可补充 |
| **AGENTS.md** | `workspace/AGENTS.md` | 每次会话开始时的行为指令 | 人类编写 |
| **TOOLS.md** | `workspace/TOOLS.md` | 本地环境特定的工具备注 | 人类编写 |
| **HEARTBEAT.md** | `workspace/HEARTBEAT.md` | 定期心跳任务列表 | 人类编写 |
| **BOOTSTRAP.md** | `workspace/BOOTSTRAP.md` | 首次会话引导（可选，用完删除） | 自动生成，一次性 |
| **MEMORY.md** | `workspace/MEMORY.md` | 长期记忆摘要 | Agent 自动维护 |

### 自定义提示词

直接编辑 `workspace/` 下的 markdown 文件即可。例如：

```bash
# 修改 Agent 的人格
vim learn-zeroclaw/workspace/SOUL.md

# 添加你的个人信息
vim learn-zeroclaw/workspace/USER.md

# 重启容器使更改生效（channel 模式下）
docker compose restart zeroclaw
```

### AIEOS 替代方案

除了 markdown 文件，还可以用 AIEOS（AI Entity Object Specification）JSON 格式：

```toml
# config.toml
[identity]
format = "aieos"
aieos_inline = '''
{
  "identity": { "names": { "first": "Nova" } },
  "psychology": { "traits": { "mbti": "ENTP" } },
  "linguistics": { "text_style": { "formality_level": 0.2 } }
}
'''
```

---

## 5. 记忆系统

### 三层记忆架构

```
┌─────────────────────────────────────────┐
│  MEMORY.md（长期记忆）                    │
│  - 人工策划的重要信息                      │
│  - 每次主会话自动注入系统提示词             │
│  - Agent 可通过 memory_store 工具写入      │
├─────────────────────────────────────────┤
│  SQLite + FTS5 + Vector（结构化记忆）      │
│  - memory_store: 存储 key-value 事实      │
│  - memory_recall: 混合检索（BM25 + 向量）  │
│  - memory_forget: 删除条目                 │
│  - 分类：Core / Daily / Conversation      │
├─────────────────────────────────────────┤
│  memory/YYYY-MM-DD.md（每日原始日志）      │
│  - 按日期自动归档                          │
│  - 按需读取（不自动注入）                   │
└─────────────────────────────────────────┘
```

### 配置

```toml
# config.toml
[memory]
backend = "sqlite"          # sqlite / markdown / lucid / none
auto_save = true            # 自动保存对话到记忆
embedding_provider = "openai"  # 嵌入模型（向量检索用）
vector_weight = 0.7         # 向量检索权重
keyword_weight = 0.3        # 关键词检索权重
```

---

## 6. 工具系统

完整 Agent 模式下可用的工具（通过 Channel 或 `zeroclaw agent` 触发）：

| 工具 | 功能 | 风险 |
|------|------|------|
| `shell` | 执行终端命令 | 高 — 受命令白名单限制 |
| `file_read` | 读取文件内容 | 中 — workspace 内 |
| `file_write` | 写入文件 | 中 — workspace 内 |
| `memory_store` | 存储记忆 | 低 |
| `memory_recall` | 检索记忆 | 低 |
| `memory_forget` | 删除记忆 | 低 |
| `http_request` | HTTP 请求 | 中 |
| `browser_open` | 打开浏览器 | 需单独开启 |
| `git_operations` | Git 操作 | 中 |
| `cron_*` | 定时任务管理 | 低 |
| `composio` | 1000+ OAuth 应用 | 需单独开启 |

### 安全策略

工具受 `[autonomy]` 配置控制：

```toml
[autonomy]
level = "supervised"    # readonly / supervised / full
workspace_only = true   # 限制在 workspace 目录内
allowed_commands = ["git", "npm", "cargo", "ls", "cat", "grep"]
```

---

## 7. 配置参考

### config.toml 完整示例

```toml
# 基本配置
workspace_dir = "/zeroclaw-data/workspace"
api_key = ""                                    # 由环境变量 API_KEY 覆盖
default_provider = "openrouter"
default_model = "openai/gpt-4o-mini"
default_temperature = 0.7

# 记忆
[memory]
backend = "sqlite"
auto_save = true

# 网关
[gateway]
port = 3000
host = "[::]"
allow_public_bind = true
require_pairing = true

# 自治级别
[autonomy]
level = "supervised"
workspace_only = true
allowed_commands = ["git", "ls", "cat", "grep"]

# 心跳（定期任务）
[heartbeat]
enabled = false
interval_minutes = 30

# 隧道（公网暴露）
[tunnel]
provider = "none"       # cloudflare / tailscale / ngrok / custom

# 浏览器
[browser]
enabled = false
allowed_domains = ["docs.rs"]

# 身份
[identity]
format = "openclaw"     # openclaw (markdown) / aieos (JSON)
```

### .env 文件

```bash
API_KEY=sk-or-v1-your-key-here
PROVIDER=openrouter
```

---

## 8. 常用操作

### 启动/停止

```bash
cd learn-zeroclaw

docker compose up -d          # 启动
docker compose down           # 停止
docker compose restart        # 重启全部
docker compose restart zeroclaw  # 只重启后端
```

### 查看日志

```bash
docker logs zeroclaw-learn              # 查看后端日志
docker logs zeroclaw-learn -f           # 实时跟踪日志
docker logs zeroclaw-learn 2>&1 | grep -A3 'one-time code'  # 获取配对码
```

### 进入容器交互

```bash
# 使用完整 Agent 模式（带工具+记忆+身份）
docker exec -it zeroclaw-learn zeroclaw agent

# 单条消息模式
docker exec -it zeroclaw-learn zeroclaw agent -m "你好"

# 查看系统状态
docker exec -it zeroclaw-learn zeroclaw status

# 运行诊断
docker exec -it zeroclaw-learn zeroclaw doctor
```

### 修改身份/提示词

```bash
# 直接编辑本地文件（已挂载到容器内）
vim workspace/SOUL.md       # 修改人格
vim workspace/USER.md       # 修改用户信息
vim workspace/IDENTITY.md   # 修改身份

# Channel 模式需要重启才生效
docker compose restart zeroclaw
```

### 重建（修改前端代码后）

```bash
docker compose up -d --build web-ui   # 只重建前端
docker compose up -d --build          # 重建全部
```

---

## 9. Gateway API 端点

| 端点 | 方法 | 认证 | 说明 |
|------|------|------|------|
| `/health` | GET | 无 | 健康检查，返回组件状态 |
| `/pair` | POST | `X-Pairing-Code` 头 | 用 6 位码换取 Bearer Token |
| `/webhook` | POST | `Authorization: Bearer <token>` | 发消息 `{"message": "..."}` 返回 LLM 回复 |
| `/whatsapp` | GET | Query params | Meta webhook 验证 |
| `/whatsapp` | POST | Meta 签名 | WhatsApp 消息接收 |

### 示例

```bash
# 健康检查
curl http://localhost:8080/api/health

# 配对
curl -X POST http://localhost:8080/api/pair \
  -H "X-Pairing-Code: 123456"

# 发消息
curl -X POST http://localhost:8080/api/webhook \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer zc_xxx..." \
  -d '{"message": "Hello!"}'
```

---

## 10. 架构限制与进阶

### 当前限制

- **Web UI `/webhook`** 走 `simple_chat()`，只有纯 LLM 对话，无工具/记忆/身份
- 完整 Agent Loop 仅在 **Channel Listener**（Telegram/Discord 等）和 **`zeroclaw agent` CLI** 中可用

### 进阶：接入 Telegram 获得完整能力

1. 创建 Telegram Bot（找 @BotFather）
2. 在 config.toml 中添加：
   ```toml
   [channels_config.telegram]
   bot_token = "your-telegram-bot-token"
   allowed_users = ["your_telegram_username"]
   ```
3. 重启：`docker compose restart zeroclaw`
4. 向你的 Bot 发消息 → 走完整 Agent Loop（工具+记忆+身份）

### 进阶：容器内交互式 Agent

```bash
docker exec -it zeroclaw-learn zeroclaw agent
```

这会启动完整的交互式 Agent，包含所有能力。
