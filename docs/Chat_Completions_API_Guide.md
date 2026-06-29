# ZeroClaw Chat Completions API 使用指南
> **文档版本**: v3.0
> **创建日期**: 2026-05-15
> **更新日期**: 2026-06-26
> **项目**: ZeroClaw v0.8.0-beta
> **接口**: `POST /v1/chat/completions`
> **兼容性**: OpenAI Chat Completions API 兼容

---

## 零、参数快速速查表

### 请求参数汇总

#### HTTP Header 参数

| 参数名 | 类型 | 必填 | 默认值 | 说明 |
|--------|------|------|--------|------|
| `Authorization` | string | ❌ | - | Bearer Token 认证（`Bearer <token>`），由配置开关 `require_pairing` 决定 |
| `x-session-key` | string | ❌ | - | 会话 ID。建议客户端生成并携带以保持多轮对话上下文 |
| `x-zeroclaw-model` | string | ❌ | - | 后端 wire model 覆盖（不切换 provider） |

#### 请求体参数（JSON Body）

| 参数名 | 类型 | 必填 | 默认值 | 范围/枚举 | 说明 |
|--------|------|------|--------|-----------|------|
| **`model`** | string | ❌ | 默认 agent | `zeroclaw/<alias>` 或普通 label | **Agent 路由目标**（非 provider 模型名）。`"zeroclaw"` / `""` / 普通 label → 默认 agent |
| **`messages`** | array | ✅ | - | 至少 1 条 | 对话消息列表。支持多轮对话（历史消息会被分流为 conversation history） |
| `stream` | boolean | ❌ | `false` | `true`/`false` | 是否流式响应（SSE） |
| `temperature` | float | ❌ | `0.7` | `0.0`~`2.0` | 采样温度 |
| `max_tokens` | integer | ❌ | - | 正整数 | 最大生成 token 数 |
| `top_p` | float | ❌ | - | `0.0`~`1.0` | Nucleus 采样 |
| `stop` | string/array | ❌ | - | 字符串列表 | 停止序列 |
| `presence_penalty` | float | ❌ | - | `-2.0`~`2.0` | 存在惩罚 |
| `frequency_penalty` | float | ❌ | - | `-2.0`~`2.0` | 频率惩罚 |
| `tools` | array | ❌ | - | 工具对象列表 | 本次对话可用的工具定义 |
| `tool_choice` | string/object | ❌ | `auto` | `"auto"`/`"none"`/`"required"`/`{"type":"function",...}` | 工具选择策略 |
| `stream_options` | object | ❌ | - | `{"include_usage": true}` | 流式选项（控制 usage 报告） |

**消息对象结构**（`messages[]`）：

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `role` | string | ✅ | `system` / `developer` / `user` / `assistant` / `tool` / `function` |
| `content` | string | ✅ | 消息内容（仅支持 string 类型） |
| `name` | string | ❌ | 可选名称 |
| `tool_calls` | array | ❌ | assistant 角色的工具调用 |
| `tool_call_id` | string | ❌ | tool 角色的工具调用 ID |

---

### 响应参数汇总

#### 非流式响应（JSON）

| 字段 | 类型 | 说明 |
|------|------|------|
| **`id`** | string | 完成请求的唯一标识符（如：`chatcmpl-<uuid>`） |
| **`object`** | string | 对象类型，始终为 `"chat.completion"` |
| **`created`** | integer | Unix 时间戳（秒） |
| **`model`** | string | 回显请求中的 model 值（空字符串归一为 `"zeroclaw"`） |
| **`choices`** | array | 生成的候选答案列表 |
| **`usage`** | object | token 使用统计（通过 cost tracking 获取，未配置时可能全为 0） |

**`choices[]` 对象结构**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `index` | integer | 选择索引（通常为 0） |
| `message` | object | 助手消息 |
| `finish_reason` | string | 停止原因（始终为 `"stop"`，即使内部调用了工具） |

**`message` 对象结构**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `role` | string | 始终为 `"assistant"` |
| `content` | string | 助手回答内容 |
| `tool_calls` | array\|null | 非流式模式下，从 agent history 中提取的工具调用；无工具调用时为 `null` |

**`usage` 对象结构**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `prompt_tokens` | integer | 输入 token 数（通过 cost tracking 采集，未配置时为 0） |
| `completion_tokens` | integer | 输出 token 数（通过 cost tracking 采集，未配置时为 0） |
| `total_tokens` | integer | 总消耗 token 数 |

---

#### 流式响应（SSE）

**SSE 事件流格式**：

```
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[{"index":0,"delta":{"content":"你"},"finish_reason":null}]}
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
data: {"id":"chatcmpl-<uuid>","object":"chat.completion.chunk","created":...,"model":"zeroclaw","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}
data: [DONE]
```

**流式响应字段说明**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | string | 完成请求 ID（与非流式相同） |
| `object` | string | 始终为 `"chat.completion.chunk"` |
| `created` | integer | Unix 时间戳 |
| `model` | string | 回显请求中的 model 值（空字符串归一为 `"zeroclaw"`） |
| `choices[]` | array | 流式分块 |
| `choices[].delta` | object | 增量内容（`role` / `content` / `tool_calls`） |
| `choices[].finish_reason` | string | 停止原因（始终为 `"stop"`） |
| `usage` | object | token 统计（仅当 `stream_options.include_usage: true` 时返回） |

---

### 响应 Header 汇总

| Header 名 | 说明 | 出现条件 |
|-----------|------|---------|
| `Content-Type` | `application/json` 或 `text/event-stream` | 始终 |
| `X-Request-ID` | 请求唯一标识符（UUID） | 始终 |
| `x-session-key` | 会话 ID（不带 `gw_` 前缀） | **始终返回**（无论客户端是否提供） |
| `X-RateLimit-Limit` | 每分钟最大请求数 | 始终 |
| `X-RateLimit-Remaining` | 剩余可用请求数 | 始终 |
| `X-RateLimit-Reset` | 速率限制重置时间戳 | 始终 |
| `Retry-After` | 429 错误时重试等待秒数 | 仅 429 响应 |

---

### 错误响应汇总

| HTTP 状态码 | error.type | 说明 | 常见原因 |
|------------|------------|------|----------|
| **400** | `invalid_request_error` | 无效请求 | messages 为空、JSON 格式错误、未知 agent、格式错误的 agent target |
| **401** | `authentication_error` | 认证失败 | token 缺失或无效（仅 `require_pairing=true` 时） |
| **429** | `rate_limit_error` | 速率限制 | 请求超过 `chat_rate_limit_per_minute` 配置值 |
| **500** | `internal_error` | 服务器内部错误 | Provider API 异常 |
| **503** | `server_error` | Agent 未配置 | agent 缺少模型配置（需完成 onboarding） |

**错误响应格式**：
```json
{
  "error": {
    "message": "错误描述信息",
    "type": "错误类型",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

---

## 一、快速开始

### 1.1 基础调用示例

```bash
# 最简单的非流式调用（不指定 model，使用默认 agent）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "你好！"}
    ]
  }'

# 指定 agent（zeroclaw/<alias> 格式）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "zeroclaw/coding",
    "messages": [
      {"role": "user", "content": "写一个快速排序"}
    ]
  }'

# 使用任意 model 名称（兼容标准客户端，自动路由到默认 agent）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-4",
    "messages": [
      {"role": "user", "content": "你好！"}
    ]
  }'
```

### 1.2 流式调用示例

```bash
# 流式调用（实时输出，建议携带 x-session-key）
curl -N -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'x-session-key: my-session-123' \
  -d '{
    "model": "zeroclaw/default",
    "stream": true,
    "messages": [
      {"role": "user", "content": "你好！"}
    ]
  }'
```

---

## 二、接口端点

| 属性 | 值 |
|------|-----|
| **URL** | `POST /v1/chat/completions` |
| **Content-Type** | `application/json` |
| **Accept** | `text/event-stream`（流式）或 `application/json`（非流式） |
| **认证** | Bearer Token（由 `require_pairing` 配置决定） |
| **超时** | 长运行超时（600s，与 cron 任务共享子路由） |

---

## 三、请求参数详解

### 3.1 HTTP Header 参数

#### 3.1.1 Authorization（认证令牌）

**格式**：
```
Authorization: Bearer <your-token>
```

**说明**：
- **何时必需**：当 ZeroClaw 配置中 `require_pairing=true` 时必需
- **何时可选**：当 `require_pairing=false` 时可选（调试模式）
- **验证逻辑**：配置开启时验证 token 有效性，无效返回 401

**示例**：
```bash
# 认证开启时必须带有效 token
curl -H 'Authorization: Bearer valid-token-123' ...

# 认证关闭时可以不带 token
curl ...
```

---

#### 3.1.2 x-session-key（会话控制键）

**格式**：
```
x-session-key: <session-id>
```

**说明**：
- **作用**：保持多轮对话的上下文连贯性
- **生成方式**：
  - **客户端提供**：使用客户端提供的 session_id
  - **客户端未提供**：服务端生成 UUID，并在响应 Header `x-session-key` 中返回
- **持久化**：会话历史会存储到 session backend（自动添加 `gw_` 前缀）
- **单轮 vs 多轮行为**：
  - 当 `messages` 只有 1 条时：自动加载 backend session 历史（如果有 `x-session-key`）
  - 当 `messages` 多于 1 条时：请求 messages 作为权威上下文，**不加载** backend 历史（避免重复）

**重要说明**：
```
📌 x-session-key 在 HTTP Response Header 中始终返回（流式和非流式行为一致）
📌 返回的 session_key 不带 "gw_" 前缀（如："550e8400-e29b-41d4-a716-446655440000"）
📌 服务端内部存储时会自动添加 "gw_" 前缀
📌 建议：客户端在首次请求时主动生成并携带 x-session-key，后续请求复用该值
```

**客户端生成示例**：
```javascript
// JavaScript/Node.js
const sessionId = crypto.randomUUID();

// Python
import uuid
session_id = str(uuid.uuid4())

// Bash
session_id=$(cat /proc/sys/kernel/random/uuid)
```

**多轮对话示例**：
```bash
SESSION_ID="550e8400-e29b-41d4-a716-446655440000"

# 第 1 轮对话
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "x-session-key: $SESSION_ID" \
  -d '{"model": "", "messages": [{"role": "user", "content": "我叫小明"}]}'

# 第 2 轮对话 - 复用同一 session_id（单条 message 自动加载 backend 历史）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "x-session-key: $SESSION_ID" \
  -d '{"model": "", "messages": [{"role": "user", "content": "我叫什么名字？"}]}'
# 助手会记得你叫小明
```

---

#### 3.1.3 x-zeroclaw-model（后端模型覆盖）

**格式**：
```
x-zeroclaw-model: <model-name>
```

**说明**：
- **作用**：覆盖当前 agent provider 下的实际调用模型名，**不切换 provider**
- **何时使用**：需要临时使用同一 provider 下的其他模型时
- **可选性**：完全可选，仅在 header 存在且非空时生效

**示例**：
```bash
# 使用 agent 默认 provider 但替换为其他模型
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'x-zeroclaw-model: qwen-plus' \
  -d '{
    "model": "zeroclaw/default",
    "messages": [{"role": "user", "content": "hi"}]
  }'
```

---

### 3.2 请求体参数（JSON）

#### 3.2.1 model（Agent 路由目标）

**类型**：`string`
**必填**：❌ 否（缺失走默认 agent）

**核心语义**：`model` 字段是 **agent 路由目标**，不是 provider 模型名称。与 OpenClaw 的 `openclaw/<agentId>` 语义一致，ZeroClaw 使用 `zeroclaw/<alias>` 选择目标 agent。

**解析规则**：

| `model` 值 | 路由结果 | 说明 |
|------------|---------|------|
| 缺失 / `""` | 默认 agent | `#[serde(default)]` 转为空字符串；响应 model 归一为 `"zeroclaw"` |
| `"zeroclaw"` | 默认 agent | 显式默认 |
| `"zeroclaw/default"` | 默认 agent | 显式默认 |
| `"zeroclaw/<alias>"` | `<alias>` | 路由到指定 agent |
| `"zeroclaw:<alias>"` | `<alias>` | 兼容别名格式 |
| `"agent:<alias>"` | `<alias>` | 兼容别名格式 |
| `"zeroclaw/"` / `"zeroclaw:"` / `"agent:"` | 400 错误 | 空 alias，格式错误 |
| 普通 label（如 `"gpt-4"`） | 默认 agent | **标准客户端兼容兜底**，不报错 |

**与 WebSocket 的一致性**：

| 项目 | `/ws/chat` | `/v1/chat/completions` |
|------|----------|----------------------|
| 传参方式 | `?agent=<alias>` 查询参数 | `model` 字段（如 `"zeroclaw/<alias>"`） |
| 必填 | 是 | 否（缺失/空值走默认 agent） |
| 校验 | `cfg.agent(&alias).is_none()` → 400 | 相同逻辑 |

**调用示例**：
```bash
# 使用默认 agent（空字符串、"zeroclaw" 或任意普通 label）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [{"role": "user", "content": "你好"}]
  }'
# 响应中 model 字段回显为 "zeroclaw"

# 指定特定 agent（zeroclaw/<alias> 格式）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -d '{"model": "zeroclaw/assistant", "messages": [...]}'

# 标准客户端兼容：传入任意 model 名称自动路由到默认 agent
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -d '{"model": "gpt-4o", "messages": [...]}'
# 不会报错，自动路由到默认 agent
```

**错误处理**：
| 场景 | HTTP 状态码 | 错误信息 |
|------|-----------|---------|
| agent 不存在 | 400 | `Unknown agent 'xxx' — no [agents.xxx] entry configured.` |
| agent target 格式错误 | 400 | `Invalid agent target 'zeroclaw/': missing agent alias` |
| agent 未配置 model_provider | 503 | `Agent not configured — complete onboarding at /onboard` |

---

#### 3.2.2 messages（对话消息列表）🔑 必填

**类型**：`array[object]`
**必填**：✅ 是
**约束**：数组不能为空

**消息对象结构**：
```json
{
  "role": "user",
  "content": "消息内容",
  "name": "可选名称",
  "tool_calls": "assistant 角色的工具调用",
  "tool_call_id": "tool 角色的工具调用 ID"
}
```

**支持的 role 类型**：
| role | 说明 | 示例 |
|------|------|------|
| `system` | 系统指令（提取为当前轮前缀，不持久化） | `"你是一个有帮助的助手"` |
| `developer` | 开发者指令（同 system） | `"你是一个代码助手"` |
| `user` | 用户消息 | `"你好！"` |
| `assistant` | 助手响应 | `"你好，有什么可以帮你？"` |
| `tool` | 工具调用结果 | `{"role": "tool", "tool_call_id": "call_123", "content": "天气晴朗"}` |
| `function` | 函数调用结果（自动归一化为 `tool`） | `{"role": "function", "content": "..."}` |

**多轮 messages 处理逻辑**：

请求体 `messages` 按 role 自动分流：

1. 从后向前找到最后一条 `user`/`tool`/`function` 消息作为**当前活跃轮**
2. 其余非 `system`/`developer` 消息作为**请求体历史**（通过 `agent.seed_history()` 注入）
3. `system`/`developer` 消息提取为**当前轮前缀**（拼接到当前轮 user 消息前，但不持久化到 session backend）
4. `function` role 自动归一化为 `tool`

**权威上下文规则**：

| `messages` 长度 | 是否加载 backend 历史 | 是否使用请求消息历史 |
|----------------|---------------------|------------------|
| `len() > 1` | 否（请求 messages 为权威上下文） | 是 |
| `len() == 1` | 是（如果有 `x-session-key`） | 否 |

**完整示例**：
```json
{
  "messages": [
    {"role": "system", "content": "你是一个专业的天气助手。"},
    {"role": "user", "content": "北京今天天气怎么样？"},
    {"role": "assistant", "content": "让我查询一下北京的天气..."},
    {"role": "tool", "tool_call_id": "call_abc123", "content": "北京今天晴朗，气温 25°C"},
    {"role": "user", "content": "那适合穿什么衣服呢？"}
  ]
}
```

上述请求的处理：
- `system` 消息 → 提取为当前轮前缀（不持久化）
- 第 1 条 `user` + `assistant` + `tool` → 作为历史注入 agent
- 最后 1 条 `user` → 当前活跃轮，拼接 system 前缀后发送

---

#### 3.2.3 stream（流式响应开关）

**类型**：`boolean`
**默认值**：`false`
**必填**：❌ 否

**说明**：
- `true` → 使用 SSE（Server-Sent Events）流式响应
- `false` → 使用 JSON 非流式响应（一次性返回完整结果）

---

#### 3.2.4 temperature（采样温度）

**类型**：`float`
**默认值**：`0.7`
**必填**：❌ 否

**说明**：
- 控制输出的随机性，值越高输出越随机
- 范围：`0.0`（确定性输出）~ `2.0`（高度随机）

---

#### 3.2.5 max_tokens（最大 token 数）

**类型**：`integer`
**必填**：❌ 否

**说明**：
- 限制生成的最大 token 数量
- 不设置时使用模型默认值

---

#### 3.2.6 stream_options（流式选项）

**类型**：`object`
**必填**：❌ 否

**说明**：
- `include_usage: true` → 在 SSE 流结束时返回 `usage` 块（包含 token 统计）
- 不设置时不返回 usage 信息

**示例**：
```json
{
  "stream": true,
  "stream_options": {"include_usage": true}
}
```

---

#### 3.2.7 其他参数

| 参数 | 类型 | 说明 |
|------|------|------|
| `top_p` | float | Nucleus 采样参数 |
| `stop` | string/array | 停止序列（如 `["。", "！"]`） |
| `presence_penalty` | float | 存在惩罚（`-2.0`~`2.0`） |
| `frequency_penalty` | float | 频率惩罚（`-2.0`~`2.0`） |
| `tools` | array | 工具定义列表（见 3.2.8） |
| `tool_choice` | string/object | 工具选择策略（见 3.2.9） |

> **注意**：`stop`、`presence_penalty`、`frequency_penalty` 等参数会被接收，但其效果取决于底层 LLM provider 是否支持。

---

#### 3.2.8 tools（动态工具定义）

**类型**：`array[object]`
**必填**：❌ 否

**说明**：
- **作用**：动态指定本次对话可用的工具子集（从 agent 已配置工具中过滤）
- **透明执行**：ZeroClaw 采用工具自动执行模式，工具调用对客户端透明
- **过滤逻辑**：仅请求中与 agent 已配置工具名称匹配的工具生效

**工具对象结构**：
```json
{
  "type": "function",
  "function": {
    "name": "工具名称",
    "description": "工具描述",
    "parameters": {
      "type": "object",
      "properties": { ... },
      "required": [...]
    }
  }
}
```

---

#### 3.2.9 tool_choice（工具选择策略）

**类型**：`string` 或 `object`
**默认值**：`auto`
**必填**：❌ 否

**支持的枚举值**：

| 值 | 说明 | ZeroClaw 行为 |
|------|------|------|
| `"auto"` | 模型自主决定是否调用工具 | 默认行为，模型根据问题判断 |
| `"none"` | 禁用所有工具 | 纯文本对话 |
| `"required"` | 应该调用工具 | 提示模型应调用至少一个工具（依赖模型遵守） |
| `{"type":"function","function":{"name":"..."}}` | 指定特定工具 | 限制可用工具为该函数（需同时提供 `tools`） |

---

## 四、响应格式

### 4.1 非流式响应（JSON）

**响应结构**：
```json
{
  "id": "chatcmpl-a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "object": "chat.completion",
  "created": 1749560000,
  "model": "zeroclaw",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "你好！有什么可以帮你？"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 20,
    "total_tokens": 30
  }
}
```

**字段说明**：
| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | `string` | 请求唯一标识符（`chatcmpl-<uuid>`） |
| `object` | `string` | 始终为 `"chat.completion"` |
| `created` | `integer` | Unix 时间戳（秒） |
| `model` | `string` | 回显请求 model（空字符串归一为 `"zeroclaw"`） |
| `choices` | `array` | 候选答案列表 |
| `usage` | `object` | token 统计（通过 cost tracking 采集，未配置时可能全为 0） |

**finish_reason 说明**：
- **ZeroClaw 行为**：始终返回 `"stop"`（即使内部调用了工具）
- **原因**：ZeroClaw 采用透明执行模式，自动执行所有工具调用直到得到最终文本响应
- **tool_calls**：非流式模式下从 agent history 提取工具调用信息；无工具调用时为 `null`

---

### 4.2 流式响应（SSE）

**响应格式**：
```
HTTP/1.1 200 OK
Content-Type: text/event-stream
Cache-Control: no-cache
Connection: keep-alive
X-Request-ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
x-session-key: 550e8400-e29b-41d4-a716-446655440000
X-RateLimit-Limit: 10
X-RateLimit-Remaining: 9
X-RateLimit-Reset: 1749560060

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","created":1749560000,"model":"zeroclaw","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","created":1749560000,"model":"zeroclaw","choices":[{"index":0,"delta":{"content":"你"},"finish_reason":null}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","created":1749560000,"model":"zeroclaw","choices":[{"index":0,"delta":{"content":"好"},"finish_reason":null}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","created":1749560000,"model":"zeroclaw","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-...","object":"chat.completion.chunk","created":1749560000,"model":"zeroclaw","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}

data: [DONE]
```

**SSE 事件类型**：
1. **首帧**：`delta.role: "assistant"` + `delta.content: ""`
2. **内容块**：`delta.content` 包含增量文本
3. **Thinking 块**（如触发）：`delta.content` 包含思考过程
4. **工具调用块**（如触发）：`delta.tool_calls` 包含工具调用信息
5. **结束帧**：`delta: {}` + `finish_reason: "stop"`
6. **Usage 块**（可选）：包含 `usage` 统计（需 `stream_options.include_usage: true`）
7. **结束标记**：`data: [DONE]`

---

## 五、错误处理

### 5.1 错误响应格式

```json
{
  "error": {
    "message": "错误描述信息",
    "type": "错误类型",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

### 5.2 常见错误类型

| HTTP 状态码 | error.type | 说明 | 解决方案 |
|------------|------------|------|----------|
| **400** | `invalid_request_error` | 无效请求 | 检查请求参数格式 |
| **400** | `invalid_request_error` | 未知 agent | 使用配置文件中存在的 agent 别名 |
| **400** | `invalid_request_error` | agent target 格式错误 | 使用正确格式（如 `zeroclaw/<alias>`，不能为空 alias） |
| **400** | `invalid_request_error` | messages 为空 | 提供至少一条消息 |
| **401** | `authentication_error` | 认证失败 | 提供有效的 Bearer Token |
| **429** | `rate_limit_error` | 速率限制 | 等待后重试（查看 `Retry-After` Header） |
| **500** | `internal_error` | 服务器内部错误 | 联系管理员，提供 `X-Request-ID` |
| **503** | `server_error` | Agent 未配置 | 完成 onboarding 或检查 agent 的 model_provider 配置 |

### 5.3 错误示例

**400 错误 - messages 为空**：
```json
{
  "error": {
    "message": "messages must not be empty",
    "type": "invalid_request_error",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

**400 错误 - 未知 agent**：
```json
{
  "error": {
    "message": "Unknown agent `nonexistent` — no [agents.nonexistent] entry configured.",
    "type": "invalid_request_error",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

**400 错误 - agent target 格式错误**：
```json
{
  "error": {
    "message": "Invalid agent target `zeroclaw/`: missing agent alias",
    "type": "invalid_request_error",
    "code": null,
    "param": null,
    "status": 400
  }
}
```

**401 错误 - 认证失败**：
```json
{
  "error": {
    "message": "Invalid or missing authentication token",
    "type": "authentication_error",
    "code": null,
    "param": null,
    "status": 401
  }
}
```

**503 错误 - Agent 未配置**：
```json
{
  "error": {
    "message": "Agent not configured — complete onboarding at /onboard",
    "type": "server_error",
    "code": null,
    "param": null,
    "status": 503
  }
}
```

**429 响应 Header**：
```
HTTP/1.1 429 Too Many Requests
Content-Type: application/json
Retry-After: 60
X-Request-ID: <uuid>
X-RateLimit-Limit: 10
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1749560060
```

---

## 六、响应 Header 说明

### 6.1 标准 Header

| Header | 说明 | 示例 |
|--------|------|------|
| `Content-Type` | 响应内容类型 | `application/json` 或 `text/event-stream` |
| `X-Request-ID` | 请求唯一标识符（UUID 格式） | `a1b2c3d4-e5f6-7890-abcd-ef1234567890` |

### 6.2 会话控制 Header

| Header | 说明 | 出现条件 |
|--------|------|----------|
| `x-session-key` | 会话 ID（不带 `gw_` 前缀） | **始终返回** |

**示例**：
```bash
# 非流式首次请求（未提供 x-session-key）
curl -sS -D - -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model": "", "messages": [{"role": "user", "content": "Hello!"}]}'

# 响应 Header 会包含：
# x-session-key: 550e8400-e29b-41d4-a716-446655440000
```

### 6.3 速率限制 Header

| Header | 说明 | 示例 |
|--------|------|------|
| `X-RateLimit-Limit` | 每分钟允许的最大请求数（配置项 `chat_rate_limit_per_minute`） | `10` |
| `X-RateLimit-Remaining` | 当前剩余可用请求数 | `9` |
| `X-RateLimit-Reset` | 速率限制重置的 Unix 时间戳 | `1749560060` |
| `Retry-After` | 429 时重试等待秒数（固定 60s） | `60` |

---

## 七、实战场景

### 场景 1：单轮对话（非流式）

```bash
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "你好，请介绍一下你自己"}
    ]
  }'
```

---

### 场景 2：多轮对话（带会话保持，单条 message 自动加载历史）

```bash
#!/bin/bash

SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
echo "Session ID: $SESSION_ID"

# 第 1 轮：用户自我介绍
echo "=== 第 1 轮 ==="
curl -sS -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "我叫小明，今年 25 岁"}
    ]
  }' | jq '.choices[0].message.content'

# 第 2 轮：询问用户信息（单条 message 自动加载 backend 历史）
echo "=== 第 2 轮 ==="
curl -sS -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "messages": [
      {"role": "user", "content": "我叫什么名字？"}
    ]
  }' | jq '.choices[0].message.content'
# 输出会包含"小明"
```

---

### 场景 3：多轮对话（带完整历史，请求 messages 作为权威上下文）

```bash
#!/bin/bash

# 在请求中直接提供完整对话历史（不需要 x-session-key）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [
      {"role": "system", "content": "你是一个友好的助手。"},
      {"role": "user", "content": "我叫小明，今年 25 岁"},
      {"role": "assistant", "content": "你好小明！"},
      {"role": "user", "content": "我叫什么名字？"}
    ]
  }' | jq '.choices[0].message.content'
# system 消息自动提取为当前轮前缀（不持久化）
# 请求 messages 作为权威上下文，不加载 backend 历史
```

---

### 场景 4：流式输出（实时显示）

```bash
#!/bin/bash

SESSION_ID=$(cat /proc/sys/kernel/random/uuid)

curl -N -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "stream": true,
    "messages": [{"role": "user", "content": "请写一首关于春天的诗"}]
  }' | while read -r line; do
    if [[ $line == data:* ]]; then
      content=$(echo "$line" | sed 's/^data: //' | jq -r '.choices[0].delta.content // empty')
      if [[ -n $content ]]; then
        echo -n "$content"
      fi
    fi
  done
```

---

### 场景 5：指定 Agent 路由

```bash
#!/bin/bash

# 使用 model 字段指定 agent（zeroclaw/<alias> 格式）
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "zeroclaw/coding",
    "messages": [{"role": "user", "content": "写一个快速排序"}]
  }'

# 如果配置了多个 agent，可以指定不同的 agent
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "zeroclaw/assistant",
    "messages": [{"role": "user", "content": "你好"}]
  }'

# 标准客户端兼容：任意 model 名称路由到默认 agent
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "你好"}]
  }'
```

---

### 场景 6：后端模型覆盖（x-zeroclaw-model）

```bash
#!/bin/bash

# 使用 agent 默认 provider，但切换为其他模型
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H 'x-zeroclaw-model: qwen-plus' \
  -d '{
    "model": "zeroclaw/default",
    "messages": [{"role": "user", "content": "你好"}]
  }'
```

---

### 场景 7：流式响应 + Usage 统计

```bash
#!/bin/bash

# 流式调用并获取 token 使用统计
curl -N -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "stream": true,
    "stream_options": {"include_usage": true},
    "messages": [{"role": "user", "content": "你好"}]
  }'
# 最后会多一个 usage 块：
# data: {"id":"chatcmpl-...","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}
```

---

### 场景 8：临时禁用所有工具

```bash
curl -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "",
    "messages": [{"role": "user", "content": "你好，请介绍一下你自己"}],
    "tool_choice": "none"
  }'
```

---

### 场景 9：多轮对话中的工具调用

```bash
#!/bin/bash

SESSION_ID=$(cat /proc/sys/kernel/random/uuid)

# 第 1 轮：查询天气
echo "=== 第 1 轮：查询天气 ==="
curl -sS -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "messages": [{"role": "user", "content": "北京今天天气怎么样？"}],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "weather_query",
          "description": "查询天气",
          "parameters": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
          }
        }
      }
    ]
  }' | jq '.choices[0].message.content'

# 第 2 轮：基于天气继续对话（禁用工具，单条 message + session 自动加载历史）
echo "=== 第 2 轮：基于天气继续对话 ==="
curl -sS -X POST http://127.0.0.1:3000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "x-session-key: $SESSION_ID" \
  -d '{
    "model": "",
    "messages": [{"role": "user", "content": "那适合穿什么衣服呢？"}],
    "tool_choice": "none"
  }' | jq '.choices[0].message.content'
```

---

## 八、常见问题（FAQ）

### Q1: ZeroClaw 的 tool_calls 为什么在非流式模式下有时为 null？

**答**：ZeroClaw 采用**透明执行模式**——工具调用由后端自动执行，客户端只会收到最终的文本响应。非流式模式下，如果 agent 在内部执行了工具调用，会从 agent history 中提取工具调用信息填充 `tool_calls` 字段；无工具调用时为 `null`。

- **非流式模式**：`finish_reason` 始终为 `"stop"`
- **流式模式**：会通过 SSE delta 返回 `tool_calls`（用于实时展示调用过程）

---

### Q2: finish_reason 为什么总是返回 "stop"？

**答**：ZeroClaw 自动执行所有工具调用直到模型给出最终文本响应。客户端收到的是完整的、经过工具增强后的回答，因此 `finish_reason` 始终为 `"stop"`。

**对比 OpenAI API**：
- OpenAI：如果模型返回 `tool_calls`，`finish_reason` 会是 `"tool_calls"`，需要客户端自行执行工具
- ZeroClaw：自动执行工具，客户端只需关注最终结果

---

### Q3: model 参数的具体路由逻辑是什么？

**答**：`model` 字段是 **agent 路由目标**，不是 provider 模型名称：

```
model = "zeroclaw/<alias>"  → 路由到指定 agent
model = "" / "zeroclaw" / 普通 label（如 "gpt-4"） → 路由到默认 agent
model = "zeroclaw/"（空 alias） → 400 错误
```

如果需要覆盖后端实际调用的模型名称，使用 `x-zeroclaw-model` header（不切换 provider）。

---

### Q4: x-agent-id header 还能用吗？

**答**：v0.8.0-beta P0 对齐后，`x-agent-id` header **已移除**。Agent 路由统一通过 `model` 字段完成。如需指定 agent，请使用 `"zeroclaw/<alias>"` 格式。

---

### Q5: 多轮对话的 system prompt 会被持久化吗？

**答**：不会。`system` 和 `developer` 消息会被提取为当前轮的前缀（拼接到当前 user 消息前发送给模型），但**不会持久化到 session backend**。这避免了后续轮次被重复的 system prompt 污染。

---

### Q6: 请求中带多条 messages 和带 x-session-key 有什么区别？

**答**：

| 方式 | 行为 |
|------|------|
| `messages` 长度 > 1 | 请求 messages 作为**权威上下文**，不加载 backend 历史；非 system/developer 的历史消息通过 `seed_history()` 注入 agent |
| `messages` 长度 == 1 + `x-session-key` | 自动加载 backend session 历史，适合逐轮追加的对话模式 |

两种方式可以混合使用：客户端可以根据场景选择"提供完整历史"或"依赖 session 自动加载"。

---

### Q7: usage 统计为什么有时全是 0？

**答**：token 使用统计通过 **cost tracking** 机制采集。如果：
- 未配置 cost tracker（`cost_tracker` 为空）
- Provider 未返回 token 计数

则 `usage` 中所有字段为 0。这不影响对话功能，只是缺少统计信息。

---

### Q8: 速率限制的配置在哪里？

**答**：在 ZeroClaw 配置文件中：

```toml
[gateway]
chat_rate_limit_per_minute = 10  # 每分钟最大请求数，默认 60
```

超过限制后返回 429，`Retry-After` Header 指示等待时间（60 秒窗口）。

---

### Q9: 如何处理 503 Agent 未配置错误？

**答**：此错误表示指定 agent 缺少模型配置。需要：

1. 确认 `[agents.<alias>]` 段存在且 `model_provider` 已配置
2. 确认 `model_provider` 指向的 provider 段有 `model` 字段
3. 或访问 `/onboard` 完成 onboarding 流程

---

## 九、最佳实践建议

### 1. 会话管理
- **客户端生成 session_key**：尤其在流式模式下，客户端应主动生成 UUID 并携带 `x-session-key`
- **复用 session**：多轮对话使用同一 `x-session-key` 保持上下文（配合单条 message 模式）
- **请求完整历史**：对于无状态客户端，可直接在 `messages` 中提供完整对话历史（> 1 条），无需 session
- **避免过长的会话历史**：过长的历史会影响 token 消耗和响应速度

### 2. Agent 和 Model 使用
- **通过 model 路由**：使用 `"zeroclaw/<alias>"` 格式指定目标 agent
- **标准客户端兼容**：Open WebUI/LobeChat 等前端传入任意 model 名称也能正常工作（自动路由到默认 agent）
- **后端模型覆盖**：需要临时切换模型时使用 `x-zeroclaw-model` header
- **无需关心 provider 模型名**：客户端只需关心 agent 别名，不需要知道后端实际使用的 provider 模型

### 3. 流式 vs 非流式
- **长文本/实时场景**：使用 `stream: true`，实时显示生成内容
- **短问答/API 集成**：使用 `stream: false`，一次性获取完整响应
- **需要 usage 统计**：流式模式下设置 `stream_options.include_usage: true`

### 4. 多轮对话选择
- **简单对话**：使用 `x-session-key` + 单条 message，依赖 backend 自动加载历史
- **精确控制上下文**：在请求中直接提供完整 `messages` 数组（> 1 条），确保上下文一致
- **system prompt**：通过 `system` role 消息传递，不影响 session 持久化

### 5. 性能优化
- **限制工具数量**：每次请求只提供必要的工具（减少 token 消耗）
- **合理设置 temperature**：日常对话 `0.7`，创意写作 `1.0`，代码生成 `0.2`
- **注意速率限制**：根据 `chat_rate_limit_per_minute` 控制请求频率

### 6. 安全建议
- **认证**：生产环境务必开启 `require_pairing=true`
- **最小权限原则**：只启用必要的工具
- **审计日志**：启用 observability 记录工具调用历史

---

## 十、版本更新说明

### v3.0（2026-06-26）— P0 对齐版

**核心变化 — model 语义修正**：
- ✅ `model` 字段现在是 **agent 路由目标**（`"zeroclaw/<alias>"`），不再是 provider 模型名
- ✅ 普通 label（如 `"gpt-4"`）自动路由到默认 agent，不再返回 400 错误
- ✅ 新增 `x-zeroclaw-model` header 用于后端 wire model 覆盖
- ❌ `x-agent-id` header **已移除**，agent 路由统一通过 `model` 字段

**核心变化 — 多轮 messages 还原**：
- ✅ `messages` 按 role 自动分流：system/developer 提取为前缀，历史消息作为 conversation history，末条 user 为当前轮
- ✅ 当 `messages.len() > 1` 时，请求 messages 作为权威上下文（不加载 backend 历史）
- ✅ 当 `messages.len() == 1` 时，自动加载 backend session 历史
- ✅ `system`/`developer` 前缀不持久化到 session backend
- ✅ `function` role 自动归一化为 `tool`

**其他改进**：
- ✅ `x-session-key` 在流式和非流式响应 Header 中**始终返回**
- ✅ 错误响应新增 `status` 字段
- ✅ `tool_calls` 在非流式模式下从 agent history 提取

### v2.0（2026-06-10）— v0.8.0 迁移版

**架构变化**：
- ✅ 新增 **onboarding 检查**（未配置 agent 返回 503）
- ✅ 新增 **agent 校验**（未知 agent 返回 400）
- ✅ 新增 **rate limiting**（`chat_rate_limit_per_minute` 配置项）
- ✅ 新增 **cost tracking**（token 使用统计通过 cost tracking 采集）
- ✅ 新增 `stream_options.include_usage` 支持
- ✅ 新增 `temperature`、`max_tokens`、`top_p`、`stop`、`presence_penalty`、`frequency_penalty` 参数支持
- ✅ `X-Request-ID` 使用 UUID 格式
- ✅ 500 错误 `error.type` 为 `internal_error`（非 `internal_server_error`）
- ✅ 请求超时使用长运行超时（600s），与 cron 任务共享子路由

### v1.2（2026-05-19）

- 支持 `tools` 和 `tool_choice` 参数
- 工具透明执行模式
- 增强的会话管理
