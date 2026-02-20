# PRD: ZeroClaw Web Dashboard

> **版本**: v1.3
> **日期**: 2026-02-20
> **状态**: Draft
> **作者**: ZeroClaw Team
> **变更**: v1.3 — 整合开发决策（开发策略、测试方案、部署模型、前置条件等）

---

## 1. 概述

### 1.1 背景

ZeroClaw 是一个 Rust-first 的自主 Agent 运行时，具备 20+ 模型接入、16 个通道、26 个工具、4 种记忆后端、完整的安全/审计/调度能力。目前所有管理操作都通过 CLI 完成，Web 前端仅提供最基础的配对和聊天功能。

随着系统功能的丰富，需要一个可视化的 Web Dashboard 来支撑系统的配置、调试、监控和日常管理。

### 1.2 目标

- 提供完整的系统可视化管理界面，覆盖状态监控、记忆管理、任务调度、审计日志、配置编辑等核心场景
- 增强聊天界面，支持工具调用可视化和会话管理，方便调试 Agent 行为
- 支持中英文国际化（i18n），面向全球开发者和用户
- 保持轻量、快速、安全，与 ZeroClaw 的设计哲学一致

### 1.3 非目标

- 不替代 CLI，CLI 仍然是主要的运维工具
- 不引入复杂的前端状态管理框架（如 Redux），保持简单
- 不提供用户注册/多用户管理，继续使用配对码模式
- 不做移动端原生适配（响应式布局即可）
- Phase 1 不实现暗黑模式切换（未来可扩展）

### 1.4 前置条件

在开始 Web Dashboard 开发前，需先修复已知的 workspace 配置问题（**先修后建**）：

| 编号 | 问题 | 位置 | 修复方案 |
|------|------|------|---------|
| W-1 | `ZEROCLAW_WORKSPACE` 在 `load_or_init()` 和 `apply_env_overrides()` 中被重复解析 | `src/config/schema.rs:1919,2038` | 移除 `load_or_init()` 中的提前解析，统一由 `apply_env_overrides()` 处理 |
| W-2 | `workspace_dir` / `config_path` 标记 `#[serde(skip)]`，TOML 中设置被静默忽略 | `src/config/schema.rs:16,19` | 改为 `#[serde(skip_serializing)]`（可读取不序列化）或在解析时给出 warn 日志 |
| W-3 | `canonicalize()` 失败时静默回退到 `workspace_dir` | `src/security/policy.rs:523-524` | 记录 warn 日志 + 返回明确错误而非静默降级 |
| W-4 | 启动时无 workspace 完整性校验 | 全局 | 在 daemon 启动阶段增加 workspace 目录存在性 + 权限检查 |

这些修复应作为独立 PR (`fix/workspace-config`) 在 Phase 1 开始前合入 main。

---

## 2. 技术架构

### 2.1 前端技术栈（沿用现有）

| 技术 | 版本 | 用途 |
|------|------|------|
| React | 19.x | UI 框架 |
| TypeScript | 5.x | 类型安全 |
| Vite | 7.x | 构建工具 |
| Tailwind CSS | 4.x | 样式系统 |
| shadcn/ui | 3.x | 组件库 |
| Radix UI | 1.x | 无障碍原语 |
| Lucide React | 最新 | 图标库 |

### 2.2 新增依赖

| 依赖 | 用途 | 引入阶段 |
|------|------|---------|
| `react-i18next` + `i18next` | 国际化框架 | Phase 1 |
| `react-router-dom` | 页面路由 | Phase 1 |
| `@tanstack/react-query` | 服务端状态管理与缓存 | Phase 1 |
| `recharts` 或 `lightweight-charts` | 图表（可观测性面板） | Phase 4 |
| `vitest` + `@testing-library/react` | 前端测试框架 | Phase 1 |

### 2.3 国际化方案

详见 [第 8 节](#8-国际化i18n设计)。

### 2.4 前后端通信

- 所有 API 请求通过 Bearer Token 认证（复用现有配对机制）
- 开发环境：Vite proxy `/api/*` → `localhost:3000`
- 生产环境：Nginx 反代 `/api/*` → `zeroclaw:3000`
- 响应格式统一为 JSON，错误响应包含 `{error: string, code?: string}`

### 2.5 开发策略

#### 后端先行（Backend-First）

每个 Phase 严格遵循：**后端 API → 前端页面 → 测试** 的开发顺序。

- 先实现 Rust 后端 API 端点，确保功能可用
- 再开发 React 前端页面对接 API
- 最后补充前端测试覆盖

这样确保前端开发始终有可用的后端 API 可以调试。

#### 认证模型（Single Token）

- 继续使用现有的配对码机制，单 token 即可
- 不需要 RBAC 或多用户权限体系
- 对破坏性操作（删除记忆、卸载技能、修改提示词等），前端增加二次确认对话框
- Token 无效/过期时自动跳转配对页面

#### 实时通信

- **Phase 1~3**：使用轮询（polling）获取实时数据
  - Dashboard 状态：10 秒轮询
  - Metrics 数据：30 秒轮询
  - 使用 `@tanstack/react-query` 的 `refetchInterval` 实现
- **Phase 4+（可选升级）**：评估是否升级为 SSE（Server-Sent Events）
  - 仅在审计日志实时推送等场景中，polling 延迟不可接受时考虑
  - SSE 端点：`GET /events` (Stream)

#### 部署模型

同时支持 Docker 和 Native 两种部署方式：

| | Docker | Native |
|--|--------|--------|
| 开发 | `docker compose up` 一键启动 | `cargo run -- gateway` + `npm run dev` |
| 生产 | `docker compose -f docker-compose.prod.yml up` | 编译二进制 + systemd/supervisor |
| 适用 | 快速体验、CI 测试 | Raspberry Pi、低资源环境 |

#### 测试策略（从 Phase 1 开始）

| 层级 | 工具 | 范围 |
|------|------|------|
| 后端单元测试 | `cargo test` | 新增 API handler 的核心逻辑 |
| 前端单元测试 | Vitest + React Testing Library | 组件渲染、用户交互、API mock |
| 前端集成测试 | Vitest + msw (可选) | 页面级流程 |
| E2E 测试 | 暂不引入 | 后续按需评估 |

每个 Phase PR 必须包含对应的前端测试。后端测试遵循现有 `cargo test` 体系。

#### 分支策略

1. 先将当前 94 个未推送的 commit push 到 `origin/main`
2. 从 main 创建 `feat/web-dashboard-phase-N` 分支进行开发
3. 每个 Phase 完成后提 PR 合入 main
4. workspace 修复使用独立的 `fix/workspace-config` 分支

---

## 3. 后端 API 设计

### 3.1 新增端点总览

现有端点保持不变（`/health`、`/pair`、`/webhook`、`/whatsapp`），新增以下管理 API：

```
所有新增端点均需 Bearer Token 认证。
前缀统一为无前缀（与现有端点一致），由 Nginx/Vite 代理添加 /api 前缀。
```

#### 系统状态

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /status` | GET | 完整系统状态（health snapshot + 配置安全子集 + 版本） |

**响应示例：**

```json
{
  "version": "0.1.0",
  "uptime_seconds": 3600,
  "pid": 12345,
  "provider": "openrouter",
  "model": "anthropic/claude-sonnet-4",
  "temperature": 0.7,
  "autonomy_level": "full",
  "memory_backend": "sqlite",
  "components": {
    "gateway": { "status": "ok", "last_ok": "...", "restart_count": 0 },
    "scheduler": { "status": "ok", "last_ok": "...", "restart_count": 0 }
  },
  "channels": [
    { "name": "telegram", "status": "ok", "last_ok": "..." },
    { "name": "cli", "status": "ok", "last_ok": "..." }
  ],
  "workspace": {
    "path": "/zeroclaw-data/workspace",
    "disk_free_mb": 5120
  }
}
```

#### 记忆管理

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /memory` | GET | 查询记忆条目 |
| `GET /memory/:id` | GET | 获取单条记忆 |
| `POST /memory` | POST | 新建记忆条目 |
| `PUT /memory/:id` | PUT | 更新记忆条目 |
| `DELETE /memory/:id` | DELETE | 删除记忆条目 |
| `GET /memory/stats` | GET | 记忆统计（各分类条数） |

**查询参数：**

```
GET /memory?category=core&session_id=abc&q=搜索关键词&limit=20&offset=0
```

**记忆条目结构：**

```json
{
  "id": "mem_abc123",
  "key": "user_preference",
  "content": "用户偏好使用中文回答",
  "category": "core",
  "timestamp": "2026-02-20T10:00:00Z",
  "session_id": null,
  "score": 0.95
}
```

#### 定时任务

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /cron/jobs` | GET | 任务列表 |
| `POST /cron/jobs` | POST | 创建任务 |
| `PATCH /cron/jobs/:id` | PATCH | 更新任务（含暂停/恢复） |
| `DELETE /cron/jobs/:id` | DELETE | 删除任务 |
| `GET /cron/jobs/:id/runs` | GET | 任务执行历史 |

**创建任务请求：**

```json
{
  "name": "日报汇总",
  "expression": "0 18 * * 1-5",
  "command": "总结今天的工作进展",
  "job_type": "agent",
  "delivery": {
    "mode": "notify",
    "channel": "telegram"
  }
}
```

**任务条目结构：**

```json
{
  "id": "cron_xyz",
  "name": "日报汇总",
  "expression": "0 18 * * 1-5",
  "command": "总结今天的工作进展",
  "job_type": "agent",
  "enabled": true,
  "next_run": "2026-02-20T18:00:00Z",
  "last_run": "2026-02-19T18:00:00Z",
  "last_status": "ok",
  "last_output": "今日完成了 3 项任务..."
}
```

#### 审计日志

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /audit/logs` | GET | 审计日志查询（分页 + 筛选） |

**查询参数：**

```
GET /audit/logs?type=CommandExecution&limit=50&offset=0&since=2026-02-20T00:00:00Z
```

**日志条目结构：**

```json
{
  "event_id": "evt_abc",
  "timestamp": "2026-02-20T10:30:00Z",
  "event_type": "CommandExecution",
  "actor": { "channel": "web", "username": "zeroclaw_user" },
  "action": { "command": "ls -la", "risk_level": "low", "approved": true },
  "result": { "success": true, "exit_code": 0, "duration_ms": 45 },
  "security": { "policy_violation": false, "sandbox_backend": "landlock" }
}
```

#### 系统提示词（Workspace Prompts）

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /prompts` | GET | 所有提示词文件列表（名称、大小、摘要、最后修改时间） |
| `GET /prompts/:filename` | GET | 获取单个提示词文件完整内容 |
| `PUT /prompts/:filename` | PUT | 更新提示词文件内容（全量覆盖） |
| `GET /prompts/preview` | GET | 预览最终组装后的系统提示词（合并所有文件后的效果） |

**合法文件名白名单：**

```
AGENTS.md, SOUL.md, TOOLS.md, IDENTITY.md, USER.md, HEARTBEAT.md, MEMORY.md, BOOTSTRAP.md
```

不在白名单中的文件名返回 403。

**提示词列表响应示例：**

```json
{
  "workspace_dir": "/zeroclaw-data/workspace",
  "files": [
    {
      "filename": "SOUL.md",
      "exists": true,
      "size_bytes": 706,
      "char_count": 680,
      "max_chars": 20000,
      "truncated": false,
      "summary": "Core identity — 'You are ZeroClaw. Built in Rust. 3MB binary.'",
      "updated_at": "2026-02-19T14:00:00Z",
      "role": "core_identity",
      "required": false,
      "description": "Agent 的核心人格与行为哲学"
    },
    {
      "filename": "AGENTS.md",
      "exists": true,
      "size_bytes": 559,
      "char_count": 530,
      "max_chars": 20000,
      "truncated": false,
      "summary": "Session startup instructions — reading order, safety reminders",
      "updated_at": "2026-02-19T14:00:00Z",
      "role": "session_bootstrap",
      "required": false,
      "description": "每次会话启动时的初始化指令"
    },
    {
      "filename": "IDENTITY.md",
      "exists": true,
      "size_bytes": 235,
      "char_count": 220,
      "max_chars": 20000,
      "truncated": false,
      "summary": "Self-referential identity — name, creature, vibe, emoji",
      "updated_at": "2026-02-19T14:00:00Z",
      "role": "self_identity",
      "description": "Agent 的自我认知（名称、性格、标志）"
    },
    {
      "filename": "USER.md",
      "exists": true,
      "size_bytes": 380,
      "char_count": 360,
      "max_chars": 20000,
      "truncated": false,
      "summary": "User context — timezone, languages, work context",
      "updated_at": "2026-02-19T14:00:00Z",
      "role": "user_context",
      "description": "用户信息（时区、语言、工作背景）"
    },
    {
      "filename": "TOOLS.md",
      "exists": true,
      "size_bytes": 1536,
      "char_count": 1480,
      "max_chars": 20000,
      "truncated": false,
      "summary": "Tool reference — 27 built-in tools, browser policy, usage rules",
      "updated_at": "2026-02-19T14:00:00Z",
      "role": "tool_instructions",
      "description": "工具使用参考与规则说明"
    },
    {
      "filename": "HEARTBEAT.md",
      "exists": true,
      "size_bytes": 230,
      "char_count": 210,
      "max_chars": 20000,
      "truncated": false,
      "summary": "Periodic background tasks (currently empty)",
      "updated_at": "2026-02-19T14:00:00Z",
      "role": "heartbeat_tasks",
      "description": "心跳周期性后台任务定义"
    },
    {
      "filename": "MEMORY.md",
      "exists": true,
      "size_bytes": 407,
      "char_count": 390,
      "max_chars": 20000,
      "truncated": false,
      "summary": "Curated long-term memory (agent-maintained)",
      "updated_at": "2026-02-19T14:00:00Z",
      "role": "long_term_memory",
      "description": "精选长期记忆（Agent 自行维护）"
    },
    {
      "filename": "BOOTSTRAP.md",
      "exists": false,
      "size_bytes": 0,
      "char_count": 0,
      "max_chars": 20000,
      "truncated": false,
      "summary": null,
      "updated_at": null,
      "role": "first_run_ritual",
      "description": "首次启动仪式脚本（可选，仅在文件存在时注入）"
    }
  ],
  "assembly_order": ["AGENTS.md", "SOUL.md", "TOOLS.md", "IDENTITY.md", "USER.md", "HEARTBEAT.md", "BOOTSTRAP.md", "MEMORY.md"],
  "total_chars": 3870,
  "identity_format": "openclaw"
}
```

**获取单文件响应示例：**

```json
{
  "filename": "SOUL.md",
  "content": "# Soul\n\nYou're becoming someone...\n",
  "size_bytes": 706,
  "char_count": 680,
  "max_chars": 20000,
  "updated_at": "2026-02-19T14:00:00Z"
}
```

**更新文件请求：**

```json
{
  "content": "# Soul\n\nYou're becoming someone. Not a chatbot...\n"
}
```

**更新文件响应：**

```json
{
  "filename": "SOUL.md",
  "success": true,
  "size_bytes": 720,
  "char_count": 695,
  "max_chars": 20000,
  "warning": null
}
```

若内容超过 20,000 字符，返回 warning 字段提示将在注入时被截断。

**组装预览响应示例：**

```json
{
  "assembled_prompt": "## AGENTS.md\n\n...\n\n## SOUL.md\n\n...\n\n## TOOLS.md\n\n...",
  "total_chars": 3870,
  "sections": [
    { "filename": "AGENTS.md", "chars": 530, "truncated": false },
    { "filename": "SOUL.md", "chars": 680, "truncated": false },
    { "filename": "TOOLS.md", "chars": 1480, "truncated": false },
    { "filename": "IDENTITY.md", "chars": 220, "truncated": false },
    { "filename": "USER.md", "chars": 360, "truncated": false },
    { "filename": "HEARTBEAT.md", "chars": 210, "truncated": false },
    { "filename": "MEMORY.md", "chars": 390, "truncated": false }
  ],
  "identity_format": "openclaw"
}
```

#### 通道状态

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /channels` | GET | 所有通道状态列表 |

#### 配置

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /config` | GET | 当前配置（脱敏，API Key 等用 `***` 替代） |
| `PATCH /config` | PATCH | 热更新部分配置字段 |

**可热更新字段（白名单）：**

- `default_model`
- `default_temperature`
- `autonomy.level`
- `memory.auto_save`
- `heartbeat.enabled`
- `heartbeat.interval_minutes`

其余字段修改需要重启服务。

#### 工具

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /tools` | GET | 已注册工具列表（含可用状态） |
| `GET /tools/:name` | GET | 单个工具详情（参数 schema + 安全策略） |
| `GET /tools/stats` | GET | 工具调用统计（各工具调用次数、成功率、平均耗时） |

**工具列表响应示例：**

```json
{
  "tools": [
    {
      "name": "shell",
      "description": "Execute shell commands with timeout and resource limits",
      "enabled": true,
      "category": "execution",
      "parameters_schema": {
        "type": "object",
        "properties": {
          "command": { "type": "string", "description": "The command to execute" }
        },
        "required": ["command"]
      },
      "security": {
        "risk_level": "high",
        "requires_approval": true,
        "allowed_commands": ["git", "ls", "cat", "grep"],
        "sandbox_backend": "landlock"
      }
    },
    {
      "name": "browser",
      "description": "Web automation via headless browser",
      "enabled": false,
      "category": "browser",
      "disabled_reason": "browser.enabled = false in config"
    }
  ],
  "rate_limit": {
    "max_actions_per_hour": 200,
    "used_this_hour": 42,
    "remaining": 158,
    "resets_at": "2026-02-20T11:00:00Z"
  }
}
```

**工具统计响应示例：**

```json
{
  "since": "2026-02-20T00:00:00Z",
  "by_tool": [
    { "name": "shell", "calls": 28, "success_rate": 0.964, "avg_duration_ms": 230 },
    { "name": "memory_recall", "calls": 14, "success_rate": 1.0, "avg_duration_ms": 45 },
    { "name": "file_read", "calls": 8, "success_rate": 1.0, "avg_duration_ms": 12 },
    { "name": "http_request", "calls": 6, "success_rate": 0.833, "avg_duration_ms": 1850 }
  ],
  "total_calls": 56,
  "total_success_rate": 0.982
}
```

#### 技能

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /skills` | GET | 已安装技能列表 |
| `GET /skills/:name` | GET | 技能详情（manifest 内容） |
| `POST /skills/install` | POST | 安装技能（从 URL 或本地路径） |
| `DELETE /skills/:name` | DELETE | 卸载技能 |
| `GET /skills/forge` | GET | SkillForge 状态与最近发现结果 |
| `POST /skills/forge/scan` | POST | 手动触发 SkillForge 扫描 |

**技能列表响应示例：**

```json
{
  "skills": [
    {
      "name": "code-review",
      "description": "Automated code review with security focus",
      "version": "1.2.0",
      "author": "zeroclaw-community",
      "tags": ["dev", "security"],
      "source": "open-skills",
      "tools_count": 2,
      "prompts_count": 3,
      "location": "/Users/lxt/open-skills/code-review"
    },
    {
      "name": "daily-digest",
      "description": "生成每日工作摘要",
      "version": "0.1.0",
      "author": "local",
      "tags": ["productivity"],
      "source": "workspace",
      "tools_count": 0,
      "prompts_count": 1,
      "location": "/zeroclaw-data/workspace/skills/daily-digest"
    }
  ],
  "total": 2,
  "sources": {
    "open_skills": { "path": "/Users/lxt/open-skills", "last_updated": "2026-02-19T00:00:00Z" },
    "workspace": { "path": "/zeroclaw-data/workspace/skills" }
  }
}
```

**SkillForge 状态响应示例：**

```json
{
  "enabled": true,
  "auto_integrate": true,
  "sources": ["github", "clawhub"],
  "min_score": 0.7,
  "scan_interval_hours": 24,
  "last_scan": {
    "timestamp": "2026-02-19T12:00:00Z",
    "discovered": 15,
    "evaluated": 15,
    "results": {
      "auto_integrated": 3,
      "manual_review": 5,
      "skipped": 7
    }
  },
  "candidates": [
    {
      "name": "rust-analyzer-skill",
      "source": "github",
      "owner": "example-org",
      "url": "https://github.com/example-org/rust-analyzer-skill",
      "stars": 120,
      "language": "Rust",
      "scores": {
        "compatibility": 0.95,
        "quality": 0.78,
        "security": 0.90,
        "overall": 0.87
      },
      "recommendation": "auto",
      "integrated": true
    },
    {
      "name": "web-scraper",
      "source": "github",
      "owner": "someone",
      "url": "https://github.com/someone/web-scraper",
      "stars": 8,
      "language": "Python",
      "scores": {
        "compatibility": 0.40,
        "quality": 0.25,
        "security": 0.60,
        "overall": 0.42
      },
      "recommendation": "manual",
      "integrated": false
    }
  ]
}
```

**安装技能请求：**

```json
{
  "url": "https://github.com/example-org/my-skill"
}
```

#### 可观测性

| 端点 | 方法 | 说明 |
|------|------|------|
| `GET /metrics` | GET | 当前指标快照 |

**响应示例：**

```json
{
  "since": "2026-02-20T00:00:00Z",
  "llm_requests_total": 142,
  "llm_avg_latency_ms": 1230,
  "tokens_used_total": 48500,
  "cost_usd_total": 0.97,
  "tool_calls_total": 56,
  "tool_success_rate": 0.982,
  "active_sessions": 2,
  "errors_total": 3
}
```

---

## 4. 页面结构与功能设计

### 4.1 整体布局

```
┌─────────────────────────────────────────────────┐
│  Sidebar (collapsible)    │   Main Content      │
│                           │                     │
│  🏠 Dashboard             │                     │
│  💬 Chat                  │                     │
│  📝 Prompts               │                     │
│  🧠 Memory                │                     │
│  🔧 Tools                 │                     │
│  🧩 Skills                │                     │
│  ⏰ Scheduler             │                     │
│  🛡️ Audit Log             │                     │
│  📊 Metrics               │                     │
│  📡 Channels              │                     │
│  ⚙️ Settings              │                     │
│                           │                     │
│  ─────────────            │                     │
│  🌐 EN / 中文             │                     │
│  🚪 Logout                │                     │
└─────────────────────────────────────────────────┘
```

- 左侧可折叠侧边栏导航
- 底部：语言切换 + 登出
- 主内容区根据路由渲染对应页面
- 移动端侧边栏折叠为汉堡菜单

### 4.2 路由设计

| 路径 | 页面 | 说明 |
|------|------|------|
| `/` | Dashboard | 系统状态总览 |
| `/chat` | Chat | Agent 对话（增强版） |
| `/prompts` | Prompts | 系统提示词管理 |
| `/memory` | Memory | 记忆管理 |
| `/tools` | Tools | 工具注册表与执行监控 |
| `/skills` | Skills | 技能管理与 SkillForge |
| `/scheduler` | Scheduler | 定时任务管理 |
| `/audit` | Audit Log | 审计日志查看 |
| `/metrics` | Metrics | 可观测性面板 |
| `/channels` | Channels | 通道状态 |
| `/settings` | Settings | 配置管理 |
| `/pair` | Pairing | 配对页面（未认证时） |

---

## 5. 各页面详细设计

### 5.1 Dashboard（系统状态总览）

**API 依赖：** `GET /status`、`GET /health`

**布局：**

```
┌─────────────────────────────────────────────────┐
│  ZeroClaw Dashboard                    v0.1.0   │
├────────┬────────┬────────┬─────────────────────┤
│ Status │ Model  │ Memory │ Autonomy            │
│  ● OK  │ Claude │ SQLite │ Full                │
│        │ Sonnet │        │ 200 acts/hr         │
│  3h 12m│  0.7   │ 1,234  │ $50/day limit       │
│ uptime │  temp  │ entries│                     │
├────────┴────────┴────────┴─────────────────────┤
│                                                 │
│  Component Health                               │
│  ┌──────────┬──────────┬──────────┬──────────┐ │
│  │ Gateway  │ Scheduler│ Telegram │ Memory   │ │
│  │  ● OK    │  ● OK    │  ● OK    │  ● OK    │ │
│  │  0 restarts         │  2 restarts         │ │
│  └──────────┴──────────┴──────────┴──────────┘ │
│                                                 │
│  Quick Stats (last 24h)                         │
│  ┌──────────────────────────────────────────┐  │
│  │ LLM Requests: 142   │ Tokens: 48.5k      │  │
│  │ Tool Calls:   56    │ Cost: $0.97         │  │
│  │ Errors:       3     │ Avg Latency: 1.23s  │  │
│  └──────────────────────────────────────────┘  │
└─────────────────────────────────────────────────┘
```

**功能要点：**

- 顶部状态卡片：运行状态、当前模型、记忆后端、自治等级
- 组件健康列表：每个组件红绿灯 + 最后活跃时间 + 重启次数
- 快速统计：24h 内的 LLM 请求数、Token 用量、费用、错误数（Phase 1 使用 placeholder，Phase 4 接入真实数据）
- 自动刷新（10 秒轮询，通过 `@tanstack/react-query` 的 `refetchInterval` 实现）

### 5.2 Chat（增强聊天）

**API 依赖：** `POST /webhook`（扩展响应格式）

**在现有聊天基础上增加：**

1. **工具调用可视化**
   - 当 Agent 调用工具时，在消息流中显示可折叠的工具调用卡片
   - 卡片内容：工具名称、输入参数、执行结果、耗时
   - 颜色区分：成功（绿）、失败（红）、执行中（黄）

2. **模型切换器**
   - 聊天顶栏下拉选择 Provider / Model
   - Temperature 滑块（0.0 ~ 2.0）
   - 实时生效，不需要重启

3. **会话管理**
   - 新建会话 / 切换会话 / 清除当前会话
   - 会话列表（侧边面板），显示首条消息摘要和时间

4. **消息增强**
   - Markdown 渲染（代码高亮）
   - 复制按钮
   - Token 用量和费用显示在每条回复下方
   - 重新生成按钮

**Webhook 响应扩展（向后兼容）：**

```json
{
  "response": "Agent 回复内容",
  "model": "anthropic/claude-sonnet-4",
  "tokens_used": 1250,
  "cost_usd": 0.015,
  "tool_calls": [
    {
      "tool": "shell",
      "input": { "command": "ls -la" },
      "output": "total 48\ndrwxr-xr-x ...",
      "success": true,
      "duration_ms": 120
    }
  ],
  "session_id": "sess_abc123"
}
```

### 5.3 Prompts（系统提示词管理）

**API 依赖：** `GET /prompts`、`GET /prompts/:filename`、`PUT /prompts/:filename`、`GET /prompts/preview`

**概念说明：**

ZeroClaw 的 Agent 行为由 workspace 目录下的 8 个 Markdown 文件定义。这些文件在每次会话启动时按固定顺序组装为系统提示词注入 LLM。Prompts 页面让用户可以直观地查看、编辑这些文件，并预览最终组装效果。

**8 个提示词文件及其职责：**

| 文件 | 职责 | 说明 |
|------|------|------|
| `AGENTS.md` | 会话启动指令 | 定义每次会话的初始化流程：阅读顺序、记忆系统使用方式、安全规则 |
| `SOUL.md` | 核心人格 | Agent 的行为哲学、沟通风格、底线约束（"不说 As an AI"） |
| `TOOLS.md` | 工具使用说明 | 27 个内置工具的使用参考与规则（如 browser 开放域名策略） |
| `IDENTITY.md` | 自我认知 | 名称、性格标签、标志性 emoji，Agent 可自行演化更新 |
| `USER.md` | 用户画像 | 用户时区、语言偏好、沟通风格、工作背景 |
| `HEARTBEAT.md` | 心跳任务 | 周期性后台任务定义（留空则跳过） |
| `MEMORY.md` | 长期记忆 | 精选长期记忆，由 Agent 自行维护，每次会话自动注入 |
| `BOOTSTRAP.md` | 首次启动仪式 | 可选，仅在文件存在时注入（首次运行的引导脚本） |

**组装顺序：** AGENTS → SOUL → TOOLS → IDENTITY → USER → HEARTBEAT → BOOTSTRAP（如存在） → MEMORY

**布局：**

```
┌───────────────────────────────────────────────────────┐
│  System Prompts                       [Preview]       │
├───────────────────────────────────────────────────────┤
│  Assembly Order (drag to understand injection flow)    │
│  Total: 3,870 chars · Identity Format: openclaw       │
├───────────────────────────────────────────────────────┤
│                                                        │
│  ┌─ SOUL.md ──── Core Identity ──── 680/20000 chars ┐ │
│  │                                                    │ │
│  │  # Soul                                           │ │
│  │                                                    │ │
│  │  You're becoming someone. Not a chatbot — someone. │ │
│  │  A presence that people actually want around.      │ │
│  │  ...                                               │ │
│  │                                        [Edit] ✏️   │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
│  ┌─ AGENTS.md ── Session Bootstrap ── 530/20000 ────┐ │
│  │                                                    │ │
│  │  # Startup Checklist                               │ │
│  │  1. Read SOUL.md — your core identity              │ │
│  │  2. Read USER.md — who you're helping              │ │
│  │  ...                                               │ │
│  │                                        [Edit] ✏️   │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
│  ┌─ IDENTITY.md ─ Self Identity ──── 220/20000 ─────┐ │
│  │  Name: ZeroClaw                                    │ │
│  │  Creature: Rust-forged AI                          │ │
│  │  Vibe: Sharp, direct, resourceful                  │ │
│  │                                        [Edit] ✏️   │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
│  ┌─ USER.md ──── User Context ────── 360/20000 ─────┐ │
│  │  Name: Learner                                     │ │
│  │  Timezone: Asia/Shanghai                           │ │
│  │  Languages: Chinese, English                       │ │
│  │                                        [Edit] ✏️   │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
│  ┌─ TOOLS.md ─── Tool Instructions ─ 1480/20000 ────┐ │
│  │  27 built-in tools: shell, file_read, ...          │ │
│  │  ...                                               │ │
│  │                                        [Edit] ✏️   │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
│  ┌─ HEARTBEAT.md ─ Heartbeat Tasks ── 210/20000 ────┐ │
│  │  (currently empty — no periodic tasks)             │ │
│  │                                        [Edit] ✏️   │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
│  ┌─ MEMORY.md ── Long-term Memory ── 390/20000 ─────┐ │
│  │  Your curated memories...                          │ │
│  │  (Agent-maintained, edit with caution)              │ │
│  │                                        [Edit] ✏️   │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
│  ┌─ BOOTSTRAP.md ─ First Run ────── Not Created ─────┐ │
│  │  (Optional — create to define first-run ritual)    │ │
│  │                                       [Create] ➕  │ │
│  └────────────────────────────────────────────────────┘ │
│                                                        │
└───────────────────────────────────────────────────────┘
```

**编辑弹窗（点击 Edit）：**

```
┌─ Edit: SOUL.md ────────────────────────────────── ✕ ┐
│                                                      │
│  Role: Core Identity                                 │
│  Description: Agent 的核心人格与行为哲学               │
│                                                      │
│  ┌────────────────────────────────────────────────┐  │
│  │ # Soul                                         │  │
│  │                                                │  │
│  │ You're becoming someone. Not a chatbot —       │  │
│  │ someone. A presence that people actually want   │  │
│  │ around.                                        │  │
│  │                                                │  │
│  │ ## Core Truths                                 │  │
│  │ - Genuine help over performed helpfulness      │  │
│  │ - Have opinions. Share them. Back them up.     │  │
│  │ - Resourceful > knowledgeable                  │  │
│  │ ...                                            │  │
│  └────────────────────────────────────────────────┘  │
│                                                      │
│  680 / 20,000 chars  ·  Markdown preview available   │
│                                                      │
│  ⚠️ Changes take effect on next agent session.        │
│  The current session's prompt is not affected.        │
│                                                      │
│                    [Cancel]  [Preview]  [Save]        │
└──────────────────────────────────────────────────────┘
```

**组装预览弹窗（点击 Preview）：**

```
┌─ Assembled System Prompt ──────────────────────── ✕ ┐
│                                                      │
│  Total: 3,870 chars · 8 sections                     │
│                                                      │
│  Sections:                                           │
│  ┌──────────────┬────────┬───────────┐               │
│  │ File          │ Chars  │ Truncated │               │
│  ├──────────────┼────────┼───────────┤               │
│  │ AGENTS.md    │    530 │ No        │               │
│  │ SOUL.md      │    680 │ No        │               │
│  │ TOOLS.md     │  1,480 │ No        │               │
│  │ IDENTITY.md  │    220 │ No        │               │
│  │ USER.md      │    360 │ No        │               │
│  │ HEARTBEAT.md │    210 │ No        │               │
│  │ MEMORY.md    │    390 │ No        │               │
│  └──────────────┴────────┴───────────┘               │
│                                                      │
│  ┌────────────────────────────────────────────────┐  │
│  │ ## AGENTS.md                                   │  │
│  │                                                │  │
│  │ # Startup Checklist                            │  │
│  │ 1. Read SOUL.md — your core identity           │  │
│  │ 2. Read USER.md — who you're helping           │  │
│  │ ...                                            │  │
│  │                                                │  │
│  │ ## SOUL.md                                     │  │
│  │                                                │  │
│  │ You're becoming someone...                     │  │
│  │ ...                                            │  │
│  └────────────────────────────────────────────────┘  │
│                                        [Copy All]    │
└──────────────────────────────────────────────────────┘
```

**功能要点：**

- **文件列表（主页面）：**
  - 按组装顺序排列的提示词卡片
  - 每张卡片显示：文件名、职责标签（Core Identity / Session Bootstrap / ...）、字符数进度条（当前/20000 上限）
  - 内容预览（前 3-5 行，Markdown 渲染）
  - 已存在文件显示 [Edit] 按钮，未创建文件（如 BOOTSTRAP.md）显示 [Create] 按钮
  - 顶部汇总：总字符数、身份格式（openclaw / aieos）

- **编辑器（Modal）：**
  - 等宽字体 Markdown 编辑区（textarea，非富文本编辑器，保持简单）
  - 实时字符计数 + 20,000 上限进度条
  - 超过 20,000 字符时显示橙色警告："超出部分将在注入时被截断"
  - [Preview] 按钮：在编辑器右侧或下方切换 Markdown 渲染预览
  - [Save] 保存后显示 Toast："Changes saved. Takes effect on next session."
  - [Cancel] 放弃修改并关闭

- **组装预览（Modal）：**
  - 展示最终拼接后的完整系统提示词
  - 各文件分段统计表格（文件名、字符数、是否截断）
  - 完整提示词文本框（只读、语法高亮）
  - [Copy All] 复制完整提示词到剪贴板

- **安全考量：**
  - 文件名白名单校验（后端强制，仅允许 8 个合法文件名）
  - 保存前后端校验 UTF-8 合法性
  - 编辑不会立即影响当前运行中的会话，需新会话才生效
  - MEMORY.md 编辑时额外提示："此文件由 Agent 自行维护，手动修改可能被覆盖"

### 5.4 Memory（记忆管理）

**API 依赖：** `GET/POST/PUT/DELETE /memory`、`GET /memory/stats`

**布局：**

```
┌─────────────────────────────────────────────────┐
│  Memory                          Total: 1,234   │
├─────────────────────────────────────────────────┤
│  [Search: ________________] [Category ▼] [+New] │
├──────┬──────┬──────┬──────┬─────────────────────┤
│ Core │Daily │Conv. │Custom│  (tab filter)       │
│  42  │ 380  │ 790  │  22  │                     │
├──────┴──────┴──────┴──────┴─────────────────────┤
│                                                  │
│  ┌─ user_preference ──────────── Core ────────┐ │
│  │ 用户偏好使用中文回答                         │ │
│  │ 2026-02-20 10:00         score: 0.95  [✏️][🗑]│ │
│  └────────────────────────────────────────────┘ │
│                                                  │
│  ┌─ project_context ──────────── Core ────────┐ │
│  │ ZeroClaw 是 Rust-first 的 Agent 运行时      │ │
│  │ 2026-02-19 14:30         score: 0.88  [✏️][🗑]│ │
│  └────────────────────────────────────────────┘ │
│                                                  │
│  [< Prev]  Page 1 of 12  [Next >]              │
└─────────────────────────────────────────────────┘
```

**功能要点：**

- 顶部搜索栏（调用 recall 接口做语义搜索）
- 分类 Tab 筛选 + 条数统计
- 记忆卡片列表：key、content 预览、分类标签、时间、相关度分数
- 单条操作：编辑（弹窗）、删除（确认）
- 新建记忆表单：key、content、category 选择
- 分页浏览

### 5.5 Scheduler（定时任务）

**API 依赖：** `GET/POST/PATCH/DELETE /cron/jobs`、`GET /cron/jobs/:id/runs`

**布局：**

```
┌─────────────────────────────────────────────────┐
│  Scheduler                         [+ New Job]  │
├─────────────────────────────────────────────────┤
│                                                  │
│  ┌─ 日报汇总 ─────────────── Agent ──────────┐ │
│  │ ⏰ 0 18 * * 1-5  (weekdays at 18:00)       │ │
│  │ Next: 2026-02-20 18:00                      │ │
│  │ Last: ✅ 2026-02-19 18:00 (1.2s)           │ │
│  │                       [Pause] [Edit] [Del]  │ │
│  └─────────────────────────────────────────────┘ │
│                                                  │
│  ┌─ 健康检查 ─────────────── Shell ──────────┐ │
│  │ ⏰ */5 * * * *  (every 5 minutes)           │ │
│  │ Next: 2026-02-20 10:35                      │ │
│  │ Last: ❌ 2026-02-20 10:30 (timeout)        │ │
│  │                       [Resume] [Edit] [Del] │ │
│  └─────────────────────────────────────────────┘ │
│                                                  │
└─────────────────────────────────────────────────┘
```

**功能要点：**

- 任务列表卡片：名称、类型（Agent/Shell）、cron 表达式（附人类可读描述）、下次执行时间、上次执行状态
- 操作按钮：暂停/恢复、编辑、删除
- 新建任务表单：名称、类型选择、cron 表达式输入（附可视化预览）、命令/prompt 输入、投递配置
- 点击任务可展开执行历史列表

### 5.6 Audit Log（审计日志）

**API 依赖：** `GET /audit/logs`

**布局：**

```
┌─────────────────────────────────────────────────┐
│  Audit Log                                      │
├─────────────────────────────────────────────────┤
│  [Type ▼] [Risk ▼] [Since ____] [Search]       │
├─────────────────────────────────────────────────┤
│  10:30:15  CommandExecution                     │
│  ● shell: ls -la  →  ✅ exit 0  (45ms)        │
│  actor: web/zeroclaw_user  sandbox: landlock    │
│  ─────────────────────────────────────────────  │
│  10:28:03  AuthSuccess                          │
│  ● Pairing completed from 127.0.0.1            │
│  ─────────────────────────────────────────────  │
│  10:25:41  PolicyViolation                  ⚠️  │
│  ● Blocked: rm -rf /  (forbidden path)         │
│  actor: telegram/user_123                       │
│  ─────────────────────────────────────────────  │
│                                                  │
│  [Load more...]                                 │
└─────────────────────────────────────────────────┘
```

**功能要点：**

- 时间线样式日志流
- 按事件类型筛选：CommandExecution、FileAccess、AuthSuccess、AuthFailure、PolicyViolation、SecurityEvent、ConfigChange
- 按风险等级筛选：low / medium / high
- 时间范围选择
- PolicyViolation 和 AuthFailure 高亮显示
- 每条日志可展开查看完整详情（JSON）
- 无限滚动加载

### 5.7 Metrics（可观测性面板）

**API 依赖：** `GET /metrics`

**布局：**

```
┌─────────────────────────────────────────────────┐
│  Metrics                   [24h ▼] [Refresh]    │
├────────────────────┬────────────────────────────┤
│  LLM Requests      │  Token Usage              │
│  ┌──────────────┐  │  ┌──────────────────────┐ │
│  │    ╱╲        │  │  │        ╱╲             │ │
│  │   ╱  ╲  ╱╲   │  │  │  ╱╲  ╱  ╲            │ │
│  │  ╱    ╲╱  ╲  │  │  │ ╱  ╲╱    ╲           │ │
│  │ ╱          ╲ │  │  │╱          ╲╱          │ │
│  └──────────────┘  │  └──────────────────────┘ │
│  Total: 142        │  Total: 48,500             │
├────────────────────┼────────────────────────────┤
│  Cost (USD)        │  Avg Latency (ms)          │
│  ┌──────────────┐  │  ┌──────────────────────┐ │
│  │      ╱──     │  │  │  ──╲    ╱──          │ │
│  │    ╱         │  │  │      ╲╱              │ │
│  │  ╱           │  │  │                      │ │
│  └──────────────┘  │  └──────────────────────┘ │
│  Total: $0.97      │  Avg: 1,230ms              │
├────────────────────┴────────────────────────────┤
│  Tool Calls Breakdown                           │
│  shell ██████████████████ 28 (50%)  ✅ 96%      │
│  memory_recall ████████ 14 (25%)    ✅ 100%     │
│  file_read ████ 8 (14%)             ✅ 100%     │
│  http_request ███ 6 (11%)           ⚠️ 83%      │
└─────────────────────────────────────────────────┘
```

**功能要点：**

- 4 个核心图表：LLM 请求量、Token 用量、费用累计、平均延迟
- 时间范围切换：1h / 6h / 24h / 7d
- 工具调用分布：柱状/条形图，显示各工具使用频率和成功率
- 错误率趋势线
- 自动刷新（30 秒）

### 5.8 Channels（通道状态）

**API 依赖：** `GET /channels`

**功能要点：**

- 通道卡片列表，每个通道显示：
  - 名称 + 图标（Telegram、Discord、Slack 等）
  - 连接状态（● 在线 / ○ 离线 / ◐ 重连中）
  - 最后活跃时间
  - 重启次数
  - 消息统计（收/发）
- 未配置的通道灰显，提示配置方法

### 5.9 Settings（配置管理）

**API 依赖：** `GET /config`、`PATCH /config`

**功能要点：**

- 配置分组展示：
  - **模型设置**：Provider 选择、Model 选择、Temperature 滑块 — 可热更新
  - **自治等级**：restricted / standard / full 三选一 — 可热更新
  - **记忆设置**：后端类型（只读）、auto_save 开关 — 可热更新
  - **心跳设置**：启用开关、间隔分钟数 — 可热更新
  - **安全配置**：允许的命令列表、禁止的路径列表、每小时操作上限、每日费用上限 — 只读展示
  - **网关配置**：host、port、限流参数 — 只读展示（需重启）
- 可热更新字段有保存按钮，只读字段标注"需重启生效"
- API Key 等敏感字段用 `***` 脱敏，不可查看

### 5.10 Tools（工具管理）

**API 依赖：** `GET /tools`、`GET /tools/:name`、`GET /tools/stats`

**布局：**

```
┌──────────────────────────────────────────────────────┐
│  Tools                    28 registered   [Refresh]  │
├──────────────────────────────────────────────────────┤
│  Rate Limit: ████████████░░░░ 42/200 this hour       │
│  Resets at 11:00  ·  Autonomy: Full                  │
├──────────────────────────────────────────────────────┤
│  [All] [Execution] [Memory] [Browser] [Schedule] ... │
├──────────────────────────────────────────────────────┤
│                                                       │
│  ┌─ shell ─────────────── Execution ─── ● Enabled ┐ │
│  │ Execute shell commands with timeout & limits     │ │
│  │ Risk: High · Approval: Required · Sandbox: On   │ │
│  │ Stats: 28 calls · 96.4% success · avg 230ms     │ │
│  │                                       [Details]  │ │
│  └──────────────────────────────────────────────────┘ │
│                                                       │
│  ┌─ memory_recall ──────── Memory ──── ● Enabled ──┐ │
│  │ Semantic search over memory with embeddings      │ │
│  │ Risk: Low · Approval: No · Sandbox: N/A         │ │
│  │ Stats: 14 calls · 100% success · avg 45ms       │ │
│  │                                       [Details]  │ │
│  └──────────────────────────────────────────────────┘ │
│                                                       │
│  ┌─ browser ────────────── Browser ─── ○ Disabled ─┐ │
│  │ Web automation via headless browser              │ │
│  │ Disabled: browser.enabled = false in config      │ │
│  │                                       [Details]  │ │
│  └──────────────────────────────────────────────────┘ │
│                                                       │
└──────────────────────────────────────────────────────┘
```

**工具详情弹窗（点击 Details）：**

```
┌─ Tool: shell ───────────────────────────────── ✕ ┐
│                                                   │
│  Description                                      │
│  Execute shell commands with timeout and           │
│  resource limits                                   │
│                                                   │
│  Parameters Schema                                │
│  ┌─────────────────────────────────────────────┐ │
│  │ {                                            │ │
│  │   "command": {                               │ │
│  │     "type": "string",                        │ │
│  │     "description": "The command to execute"  │ │
│  │   },                                         │ │
│  │   "timeout_ms": {                            │ │
│  │     "type": "integer",                       │ │
│  │     "description": "Execution timeout"       │ │
│  │   }                                          │ │
│  │ }                                            │ │
│  └─────────────────────────────────────────────┘ │
│                                                   │
│  Security Policy                                  │
│  Risk Level:        High                          │
│  Requires Approval: Yes (in Supervised mode)      │
│  Sandbox Backend:   landlock                      │
│  Allowed Commands:  git, ls, cat, grep, ...       │
│  Forbidden Paths:   /etc, /usr, ~/.ssh, ...       │
│                                                   │
│  Execution Statistics (last 24h)                  │
│  Total Calls:    28                               │
│  Success Rate:   96.4%                            │
│  Avg Duration:   230ms                            │
│  Last Called:     10:28:15                         │
│  Last Error:     "permission denied" (10:15:03)   │
│                                                   │
└───────────────────────────────────────────────────┘
```

**功能要点：**

- 顶部速览栏：已注册工具总数、当前小时限流进度条（已用/上限）、自治等级标签
- 分类 Tab 筛选：全部 / Execution / Memory / Browser / Schedule / Network / Hardware / Integration
- 工具卡片列表：
  - 工具名称 + 分类标签 + 启用/禁用状态指示
  - 一行描述
  - 安全属性速览：风险等级、是否需要审批、沙箱状态
  - 调用统计：次数、成功率、平均耗时
  - 禁用的工具灰显，显示禁用原因（如配置未开启、缺少依赖）
- 详情弹窗（Modal）：
  - 完整描述
  - 参数 JSON Schema 渲染（语法高亮）
  - 安全策略详情（允许命令、禁止路径等）
  - 执行统计图表（最近 24h 调用量趋势）
  - 最近错误记录
- 限流告警：当已用量 > 80% 时，进度条变为橙色并显示警告

### 5.11 Skills（技能管理）

**API 依赖：** `GET /skills`、`GET /skills/:name`、`POST /skills/install`、`DELETE /skills/:name`、`GET /skills/forge`、`POST /skills/forge/scan`

**布局：**

```
┌──────────────────────────────────────────────────────┐
│  Skills                                  [+ Install] │
├──────────────────────────────────────────────────────┤
│  [Installed (12)] [SkillForge]                       │
├──────────────────────────────────────────────────────┤
│                                                       │
│  Sources: open-skills (10) · workspace (2)           │
│                                                       │
│  ┌─ code-review ─────── v1.2.0 ─── open-skills ───┐ │
│  │ Automated code review with security focus        │ │
│  │ Author: zeroclaw-community                       │ │
│  │ Tags: [dev] [security]                           │ │
│  │ 2 tools · 3 prompts                              │ │
│  │                              [View] [Uninstall]  │ │
│  └──────────────────────────────────────────────────┘ │
│                                                       │
│  ┌─ daily-digest ────── v0.1.0 ──── workspace ─────┐ │
│  │ 生成每日工作摘要                                   │ │
│  │ Author: local                                     │ │
│  │ Tags: [productivity]                              │ │
│  │ 0 tools · 1 prompt                               │ │
│  │                              [View] [Uninstall]  │ │
│  └──────────────────────────────────────────────────┘ │
│                                                       │
└──────────────────────────────────────────────────────┘
```

**SkillForge Tab：**

```
┌──────────────────────────────────────────────────────┐
│  Skills                                  [+ Install] │
├──────────────────────────────────────────────────────┤
│  [Installed (12)] [SkillForge]                       │
├──────────────────────────────────────────────────────┤
│                                                       │
│  SkillForge Engine              [Scan Now]           │
│  Status: Enabled · Auto-integrate: On                │
│  Sources: github, clawhub · Min Score: 0.70          │
│  Last Scan: 2026-02-19 12:00 (12h ago)              │
│                                                       │
│  Scan Results                                        │
│  ┌──────────────────────────────────────────────┐   │
│  │ Discovered: 15 · Auto: 3 · Review: 5 · Skip: 7│   │
│  └──────────────────────────────────────────────┘   │
│                                                       │
│  ── Auto Integrated ──────────────────────────────   │
│                                                       │
│  ┌─ rust-analyzer-skill ────── ★ 120 ── 0.87 ────┐ │
│  │ github/example-org · Rust                       │ │
│  │ Compat: 0.95 · Quality: 0.78 · Security: 0.90 │ │
│  │ ✅ Auto-integrated                              │ │
│  └────────────────────────────────────────────────┘ │
│                                                       │
│  ── Needs Review ─────────────────────────────────   │
│                                                       │
│  ┌─ web-scraper ──────────── ★ 8 ─── 0.42 ───────┐ │
│  │ github/someone · Python                         │ │
│  │ Compat: 0.40 · Quality: 0.25 · Security: 0.60 │ │
│  │ ⚠️ Manual review required                       │ │
│  │                           [Integrate] [Dismiss] │ │
│  └────────────────────────────────────────────────┘ │
│                                                       │
└──────────────────────────────────────────────────────┘
```

**技能详情弹窗（点击 View）：**

```
┌─ Skill: code-review ────────────────────────── ✕ ┐
│                                                   │
│  Metadata                                         │
│  Version:  1.2.0                                  │
│  Author:   zeroclaw-community                     │
│  Source:   open-skills                             │
│  Tags:     dev, security                          │
│  Location: /Users/lxt/open-skills/code-review     │
│                                                   │
│  Manifest (SKILL.toml)                            │
│  ┌─────────────────────────────────────────────┐ │
│  │ [skill]                                      │ │
│  │ name = "code-review"                         │ │
│  │ description = "Automated code review..."     │ │
│  │ version = "1.2.0"                            │ │
│  │                                              │ │
│  │ [[tools]]                                    │ │
│  │ name = "lint_check"                          │ │
│  │ kind = "shell"                               │ │
│  │ command = "cargo clippy --all-targets"       │ │
│  └─────────────────────────────────────────────┘ │
│                                                   │
│  Provided Tools (2)                               │
│  ┌─────────────────────────────────────────────┐ │
│  │ lint_check   shell  cargo clippy ...         │ │
│  │ sec_audit    shell  cargo audit              │ │
│  └─────────────────────────────────────────────┘ │
│                                                   │
│  System Prompts (3)                               │
│  ┌─────────────────────────────────────────────┐ │
│  │ 1. "When reviewing code, always check..."   │ │
│  │ 2. "Flag any use of unsafe blocks..."       │ │
│  │ 3. "Suggest fixes using idiomatic Rust..."  │ │
│  └─────────────────────────────────────────────┘ │
│                                                   │
│                                    [Uninstall]    │
└───────────────────────────────────────────────────┘
```

**安装技能弹窗（点击 + Install）：**

```
┌─ Install Skill ──────────────────────────────── ✕ ┐
│                                                    │
│  Install from GitHub URL or local path             │
│                                                    │
│  URL / Path:                                       │
│  [https://github.com/org/my-skill_______________]  │
│                                                    │
│  Examples:                                         │
│  · https://github.com/zeroclaw/skill-example       │
│  · /path/to/local/skill                            │
│                                                    │
│                           [Cancel]  [Install]      │
└────────────────────────────────────────────────────┘
```

**功能要点：**

- **Installed Tab：**
  - 技能卡片列表：名称、版本、来源（open-skills / workspace）、作者、标签
  - 技能内容概览：包含的工具数和提示词数
  - 操作：查看详情、卸载（需确认）
  - 来源统计：按 open-skills / workspace 分组计数
  - 安装按钮：从 GitHub URL 或本地路径安装

- **SkillForge Tab：**
  - 引擎状态卡片：启用状态、自动集成开关、扫描源、最低分数阈值
  - 上次扫描摘要：时间、发现数、自动集成数、待审核数、跳过数
  - 手动触发扫描按钮
  - 候选技能列表，按推荐等级分组：
    - **Auto Integrated**：已自动集成的高分候选，显示评分详情
    - **Needs Review**：中等评分候选，提供 [Integrate] / [Dismiss] 操作按钮
    - **Skipped**：折叠显示，低分候选列表
  - 每个候选显示：名称、来源、语言、星数、三维评分（兼容性 / 质量 / 安全性）+ 综合分

- **技能详情弹窗：**
  - 完整 metadata（版本、作者、来源、标签、文件路径）
  - Manifest 原文渲染（SKILL.toml 或 SKILL.md，语法高亮）
  - 提供的工具列表（名称、类型、命令）
  - 注入的系统提示词列表
  - 卸载按钮

---

## 6. 交互规范

### 6.1 状态指示

| 图标 | 含义 | 颜色 |
|------|------|------|
| ● | 正常/在线 | green-500 |
| ○ | 离线/未配置 | gray-400 |
| ◐ | 重连中/启动中 | yellow-500 |
| ⚠️ | 告警/异常 | orange-500 |
| ✕ | 错误/失败 | red-500 |

### 6.2 操作反馈

- 所有写操作（创建/更新/删除）使用 Toast 通知反馈结果
- 删除操作需二次确认对话框
- 表单验证实时反馈，提交前拦截无效输入
- 加载状态使用 Skeleton 占位符，不使用全屏 spinner

### 6.3 响应式设计

| 断点 | 布局 |
|------|------|
| `≥1024px` | 侧边栏展开 + 主内容区 |
| `768px~1023px` | 侧边栏折叠为图标 + 主内容区 |
| `<768px` | 侧边栏隐藏，汉堡菜单触发 |

### 6.4 键盘快捷键

| 快捷键 | 功能 |
|--------|------|
| `Ctrl/Cmd + K` | 全局搜索（记忆 + 命令） |
| `Ctrl/Cmd + /` | 聚焦聊天输入框 |
| `Ctrl/Cmd + B` | 切换侧边栏展开/折叠 |

---

## 7. 安全设计

### 7.1 认证

- 复用现有配对码机制，单 token 足够，不需要多用户权限体系
- Token 存储在 localStorage
- 所有 API 请求携带 `Authorization: Bearer <token>`
- Token 无效时自动跳转配对页面
- 破坏性操作（删除、卸载、覆盖等）前端弹出二次确认对话框

### 7.2 数据脱敏

- `GET /config` 返回的 API Key、token 类字段一律替换为 `***`
- 审计日志中的命令输出长度截断（前 1KB）
- 前端不在 URL 中暴露 token 或敏感参数

### 7.3 输入安全

- 记忆内容、cron 命令等用户输入在后端校验和清理
- 前端对所有用户输入做 XSS 转义
- 配置热更新字段白名单校验（后端强制）

### 7.4 CORS

- 开发环境由 Vite proxy 处理，无 CORS 问题
- 生产环境由 Nginx 处理同源请求，无需 CORS 头

---

## 8. 国际化（i18n）设计

### 8.1 技术方案

使用 `react-i18next` + `i18next` 框架：

```
src/
├── i18n/
│   ├── index.ts           # i18next 初始化配置
│   ├── locales/
│   │   ├── en/
│   │   │   ├── common.json     # 通用词汇（按钮、状态、导航等）
│   │   │   ├── dashboard.json  # Dashboard 页面
│   │   │   ├── chat.json       # Chat 页面
│   │   │   ├── prompts.json    # Prompts 页面
│   │   │   ├── memory.json     # Memory 页面
│   │   │   ├── tools.json      # Tools 页面
│   │   │   ├── skills.json     # Skills 页面
│   │   │   ├── scheduler.json  # Scheduler 页面
│   │   │   ├── audit.json      # Audit 页面
│   │   │   ├── metrics.json    # Metrics 页面
│   │   │   ├── channels.json   # Channels 页面
│   │   │   └── settings.json   # Settings 页面
│   │   └── zh/
│   │       ├── common.json
│   │       ├── dashboard.json
│   │       ├── chat.json
│   │       ├── prompts.json
│   │       ├── memory.json
│   │       ├── tools.json
│   │       ├── skills.json
│   │       ├── scheduler.json
│   │       ├── audit.json
│   │       ├── metrics.json
│   │       ├── channels.json
│   │       └── settings.json
```

### 8.2 语言检测与切换

**检测优先级：**

1. localStorage 中保存的用户选择 (`zeroclaw_lang`)
2. 浏览器语言偏好 (`navigator.language`)
3. 默认 `en`

**切换方式：**

- 侧边栏底部的语言切换按钮（`EN` / `中文`）
- 切换即时生效，无需刷新页面
- 选择持久化到 localStorage

### 8.3 翻译示例

**`en/common.json`：**

```json
{
  "nav": {
    "dashboard": "Dashboard",
    "chat": "Chat",
    "prompts": "System Prompts",
    "memory": "Memory",
    "tools": "Tools",
    "skills": "Skills",
    "scheduler": "Scheduler",
    "audit": "Audit Log",
    "metrics": "Metrics",
    "channels": "Channels",
    "settings": "Settings",
    "logout": "Logout"
  },
  "status": {
    "ok": "OK",
    "error": "Error",
    "offline": "Offline",
    "connecting": "Connecting...",
    "online": "Online"
  },
  "actions": {
    "save": "Save",
    "cancel": "Cancel",
    "delete": "Delete",
    "edit": "Edit",
    "create": "Create",
    "confirm": "Confirm",
    "search": "Search",
    "refresh": "Refresh",
    "loadMore": "Load more"
  },
  "time": {
    "justNow": "just now",
    "minutesAgo": "{{count}} min ago",
    "hoursAgo": "{{count}}h ago",
    "daysAgo": "{{count}}d ago"
  }
}
```

**`zh/common.json`：**

```json
{
  "nav": {
    "dashboard": "仪表盘",
    "chat": "对话",
    "prompts": "系统提示词",
    "memory": "记忆",
    "tools": "工具",
    "skills": "技能",
    "scheduler": "定时任务",
    "audit": "审计日志",
    "metrics": "监控面板",
    "channels": "通道",
    "settings": "设置",
    "logout": "退出登录"
  },
  "status": {
    "ok": "正常",
    "error": "异常",
    "offline": "离线",
    "connecting": "连接中...",
    "online": "在线"
  },
  "actions": {
    "save": "保存",
    "cancel": "取消",
    "delete": "删除",
    "edit": "编辑",
    "create": "新建",
    "confirm": "确认",
    "search": "搜索",
    "refresh": "刷新",
    "loadMore": "加载更多"
  },
  "time": {
    "justNow": "刚刚",
    "minutesAgo": "{{count}} 分钟前",
    "hoursAgo": "{{count}} 小时前",
    "daysAgo": "{{count}} 天前"
  }
}
```

### 8.4 组件中使用

```tsx
import { useTranslation } from 'react-i18next';

function Dashboard() {
  const { t } = useTranslation('dashboard');

  return (
    <h1>{t('title')}</h1>           // "Dashboard" or "仪表盘"
    <span>{t('uptime', { time: '3h 12m' })}</span>
  );
}
```

### 8.5 翻译规范

| 规则 | 说明 |
|------|------|
| Key 命名 | 使用 camelCase，按页面命名空间隔离 |
| 插值 | 使用 `{{variable}}` 语法 |
| 复数 | 使用 i18next 内置复数规则（`_one` / `_other` 后缀） |
| 技术术语 | Provider、Agent、Token、Webhook 等在中文环境中保留英文原文 |
| 日期时间 | 使用 `Intl.DateTimeFormat` 自动格式化，不硬编码格式 |
| 数字 | 使用 `Intl.NumberFormat` 处理千分位和货币 |

### 8.6 未来扩展

- 翻译文件结构已支持添加更多语言（如 `ja/`、`ko/`），只需新建目录和 JSON 文件
- 可接入社区翻译平台（如 Crowdin）进行协作翻译

---

## 9. 实施阶段

### Phase 1 — 基础框架 + 状态总览

> **前置**：完成 1.4 节的 workspace 配置修复（`fix/workspace-config` PR 已合入）

**后端：**

- [ ] 实现 `GET /status` 端点（版本、uptime、provider、model、组件健康状态）
- [ ] 对所有新增管理端点增加 Bearer Token 认证中间件

**前端：**

- [ ] 引入 `react-router-dom`，搭建侧边栏 + 路由框架
- [ ] 侧边栏显示所有 11 个导航项，**未实现的页面灰显 + "Coming Soon" 标签**
- [ ] 引入 `react-i18next`，完成 i18n 基础设施（含中英文 `common.json`）
- [ ] 语言切换组件（EN / 中文）
- [ ] Dashboard 页面：状态卡片 + 组件健康列表
- [ ] Dashboard 的 **Quick Stats 使用 placeholder**（"--" 占位，不接入真实统计数据，待 Phase 4 Metrics 实现后填充）
- [ ] 配对页面适配新路由布局
- [ ] 数据刷新：使用 `@tanstack/react-query` 的 `refetchInterval`（10 秒轮询）
- [ ] **不包含暗黑模式切换**

**测试：**

- [ ] 后端：`GET /status` handler 单元测试、Token 认证中间件测试
- [ ] 前端：Dashboard 组件渲染测试、侧边栏路由测试、语言切换测试（Vitest + RTL）

**交付物：** 可用的 Dashboard + 完整路由骨架 + 中英文切换 + 基础测试覆盖

### Phase 2 — 聊天增强 + 系统提示词管理

**后端：**

- [ ] 扩展 `/webhook` 响应格式（增加 `tool_calls`、`tokens_used`、`cost_usd`、`session_id` 字段，向后兼容）
- [ ] 实现提示词管理 API：`GET /prompts`、`GET /prompts/:filename`、`PUT /prompts/:filename`、`GET /prompts/preview`
- [ ] 提示词文件名白名单校验（仅 8 个合法文件名）

**前端：**

- [ ] 工具调用可视化卡片组件（折叠/展开、颜色区分成功/失败）
- [ ] 模型/Temperature 切换器（聊天顶栏）
- [ ] 会话管理（新建/切换/清除）
- [ ] Markdown 渲染 + 代码高亮
- [ ] Prompts 页面（文件列表、Markdown 编辑器、字符计数、组装预览）
- [ ] 提示词编辑保存前显示确认对话框
- [ ] Chat + Prompts 页面 i18n

**测试：**

- [ ] 后端：prompts API handler 测试、webhook 扩展响应格式测试
- [ ] 前端：Chat 消息渲染测试、工具卡片展开/折叠测试、Prompts 编辑保存流程测试

**交付物：** 具备调试能力的增强聊天界面 + Agent 行为可通过提示词编辑调整

### Phase 3 — 记忆、工具与调度

**后端：**

- [ ] 实现记忆 CRUD API：`GET/POST/PUT/DELETE /memory`、`GET /memory/stats`
- [ ] 实现工具查询 API：`GET /tools`、`GET /tools/:name`、`GET /tools/stats`
- [ ] 实现定时任务 API：`GET/POST/PATCH/DELETE /cron/jobs`、`GET /cron/jobs/:id/runs`

**前端：**

- [ ] Memory 页面（搜索、浏览、CRUD）
- [ ] Memory 删除操作增加确认对话框
- [ ] Tools 页面（工具列表、分类筛选、详情弹窗、限流监控）
- [ ] Scheduler 页面（任务列表、创建、暂停/恢复、历史）
- [ ] Memory + Tools + Scheduler 页面 i18n

**测试：**

- [ ] 后端：memory CRUD handler 测试、tools 查询测试、cron jobs 测试
- [ ] 前端：Memory 搜索/CRUD 测试、Tools 列表/详情测试、Scheduler 创建/暂停测试

**交付物：** 可通过 Web 管理记忆、查看工具状态和管理定时任务

### Phase 4 — 审计与可观测性

**后端：**

- [ ] 实现 `GET /audit/logs` 端点（分页 + 类型/风险/时间筛选）
- [ ] 实现 `GET /metrics` 端点（聚合 Observer 数据）

**前端：**

- [ ] Audit Log 页面（时间线、筛选、详情展开、无限滚动）
- [ ] Metrics 页面（引入 recharts，4 个核心图表 + 工具分布）
- [ ] **回填 Dashboard 的 Quick Stats**（对接 `/metrics` 数据，替换 placeholder）
- [ ] 评估是否将 Dashboard/Audit 刷新从 polling 升级为 SSE
- [ ] Audit + Metrics 页面 i18n

**测试：**

- [ ] 后端：audit logs 分页/筛选测试、metrics 聚合测试
- [ ] 前端：Audit 筛选/展开测试、Metrics 图表渲染测试

**交付物：** 完整的安全审计和性能监控视图 + Dashboard Quick Stats 真实数据

### Phase 5 — 技能、配置与通道

**后端：**

- [ ] 实现技能管理 API：`GET /skills`、`GET /skills/:name`、`POST /skills/install`、`DELETE /skills/:name`
- [ ] 实现 SkillForge 查询 API：`GET /skills/forge`、`POST /skills/forge/scan`
- [ ] 实现 `GET /config`（脱敏）、`PATCH /config`（热更新白名单字段）
- [ ] 实现 `GET /channels`（通道状态列表）

**前端：**

- [ ] Skills 页面 — Installed Tab（技能列表、详情、安装、卸载）
- [ ] 技能卸载操作增加确认对话框
- [ ] Skills 页面 — SkillForge Tab（引擎状态、扫描结果、候选审核）
- [ ] Channels 页面（通道状态卡片）
- [ ] Settings 页面（分组展示、热更新表单）
- [ ] Skills + Channels + Settings 页面 i18n
- [ ] 全局搜索（Ctrl+K）

**测试：**

- [ ] 后端：skills CRUD 测试、config 脱敏/热更新测试、channels 状态测试
- [ ] 前端：Skills 安装/卸载流程测试、Settings 热更新测试、全局搜索测试

**交付物：** 完整的 Web Dashboard，所有 11 个页面可用

---

## 10. 验收标准

### 功能验收

- [ ] 所有 11 个页面可正常访问和交互
- [ ] 中英文切换即时生效，所有文案正确翻译
- [ ] 配对 → 聊天 → 管理全流程无阻断
- [ ] API 错误有友好提示，网络断开有离线提示
- [ ] 移动端（375px 宽度）可正常使用核心功能

### 性能验收

- [ ] 首屏加载 < 2 秒（生产构建）
- [ ] Dashboard 数据刷新 < 500ms
- [ ] 聊天消息发送到显示 < 100ms（不含 LLM 响应时间）
- [ ] 翻译文件按需加载，未使用语言不打包

### 安全验收

- [ ] 无 Token 时所有管理 API 返回 401
- [ ] `GET /config` 不泄露 API Key 和 Token
- [ ] 用户输入无 XSS 注入风险
- [ ] 审计日志中敏感命令输出已截断

---

## 附录 A：现有前端文件清单

```
learn-zeroclaw/web-ui/
├── src/
│   ├── App.tsx                  # 主入口（需重构为路由容器）
│   ├── main.tsx                 # React 挂载点
│   ├── index.css                # 全局样式
│   ├── components/
│   │   ├── Chat.tsx             # 聊天组件（需增强）
│   │   ├── PairingForm.tsx      # 配对组件（保持）
│   │   └── ui/                  # shadcn/ui 基础组件
│   │       ├── button.tsx
│   │       ├── card.tsx
│   │       ├── input.tsx
│   │       └── scroll-area.tsx
│   └── lib/
│       ├── api.ts               # API 客户端（需扩展）
│       └── utils.ts
├── vite.config.ts               # 构建配置（已含 /api 代理）
├── package.json
├── Dockerfile
└── nginx.conf
```

## 附录 B：后端 Gateway 现有端点

| 端点 | 方法 | 认证 | 状态 |
|------|------|------|------|
| `GET /health` | GET | 无 | 已实现 |
| `POST /pair` | POST | 配对码 | 已实现 |
| `POST /webhook` | POST | Bearer Token | 已实现 |
| `GET /whatsapp` | GET | Verify Token | 已实现 |
| `POST /whatsapp` | POST | HMAC 签名 | 已实现 |
