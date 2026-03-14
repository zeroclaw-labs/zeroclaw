# ZeroClaw 提供商参考文档

本文档映射提供商 ID、别名和凭证环境变量。

最后验证时间：**2026年2月21日**。

## 如何列出提供商

```bash
zeroclaw providers
```

## 凭证解析顺序

运行时解析顺序为：

1. 配置/CLI 中的显式凭证
2. 提供商特定的环境变量
3. 通用回退环境变量：`ZEROCLAW_API_KEY` 然后是 `API_KEY`

对于弹性回退链（`reliability.fallback_providers`），每个回退提供商独立解析凭证。主提供商的显式凭证不会重用于回退提供商。

## 提供商目录

| 标准 ID | 别名 | 本地 | 提供商特定环境变量 |
|---|---|---:|---|
| `openrouter` | — | 否 | `OPENROUTER_API_KEY` |
| `anthropic` | — | 否 | `ANTHROPIC_OAUTH_TOKEN`、`ANTHROPIC_API_KEY` |
| `openai` | — | 否 | `OPENAI_API_KEY` |
| `ollama` | — | 是 | `OLLAMA_API_KEY`（可选） |
| `gemini` | `google`、`google-gemini` | 否 | `GEMINI_API_KEY`、`GOOGLE_API_KEY` |
| `venice` | — | 否 | `VENICE_API_KEY` |
| `vercel` | `vercel-ai` | 否 | `VERCEL_API_KEY` |
| `cloudflare` | `cloudflare-ai` | 否 | `CLOUDFLARE_API_KEY` |
| `moonshot` | `kimi` | 否 | `MOONSHOT_API_KEY` |
| `kimi-code` | `kimi_coding`、`kimi_for_coding` | 否 | `KIMI_CODE_API_KEY`、`MOONSHOT_API_KEY` |
| `synthetic` | — | 否 | `SYNTHETIC_API_KEY` |
| `opencode` | `opencode-zen` | 否 | `OPENCODE_API_KEY` |
| `opencode-go` | — | 否 | `OPENCODE_GO_API_KEY` |
| `zai` | `z.ai` | 否 | `ZAI_API_KEY` |
| `glm` | `zhipu` | 否 | `GLM_API_KEY` |
| `minimax` | `minimax-intl`、`minimax-io`、`minimax-global`、`minimax-cn`、`minimaxi`、`minimax-oauth`、`minimax-oauth-cn`、`minimax-portal`、`minimax-portal-cn` | 否 | `MINIMAX_OAUTH_TOKEN`、`MINIMAX_API_KEY` |
| `bedrock` | `aws-bedrock` | 否 | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`（可选：`AWS_REGION`） |
| `qianfan` | `baidu` | 否 | `QIANFAN_API_KEY` |
| `doubao` | `volcengine`、`ark`、`doubao-cn` | 否 | `ARK_API_KEY`、`DOUBAO_API_KEY` |
| `qwen` | `dashscope`、`qwen-intl`、`dashscope-intl`、`qwen-us`、`dashscope-us`、`qwen-code`、`qwen-oauth`、`qwen_oauth` | 否 | `QWEN_OAUTH_TOKEN`、`DASHSCOPE_API_KEY` |
| `groq` | — | 否 | `GROQ_API_KEY` |
| `mistral` | — | 否 | `MISTRAL_API_KEY` |
| `xai` | `grok` | 否 | `XAI_API_KEY` |
| `deepseek` | — | 否 | `DEEPSEEK_API_KEY` |
| `together` | `together-ai` | 否 | `TOGETHER_API_KEY` |
| `fireworks` | `fireworks-ai` | 否 | `FIREWORKS_API_KEY` |
| `novita` | — | 否 | `NOVITA_API_KEY` |
| `perplexity` | — | 否 | `PERPLEXITY_API_KEY` |
| `cohere` | — | 否 | `COHERE_API_KEY` |
| `copilot` | `github-copilot` | 否 |（使用配置/`API_KEY` 回退搭配 GitHub 令牌） |
| `lmstudio` | `lm-studio` | 是 |（可选；默认本地） |
| `llamacpp` | `llama.cpp` | 是 | `LLAMACPP_API_KEY`（可选；仅当启用服务器认证时需要） |
| `sglang` | — | 是 | `SGLANG_API_KEY`（可选） |
| `vllm` | — | 是 | `VLLM_API_KEY`（可选） |
| `osaurus` | — | 是 | `OSAURUS_API_KEY`（可选；默认为 `"osaurus"`） |
| `nvidia` | `nvidia-nim`、`build.nvidia.com` | 否 | `NVIDIA_API_KEY` |

### Vercel AI Gateway 说明

- 提供商 ID：`vercel`（别名：`vercel-ai`）
- 基础 API URL：`https://ai-gateway.vercel.sh/v1`
- 认证：`VERCEL_API_KEY`
- Vercel AI Gateway 使用不需要项目部署。
- 如果你看到 `DEPLOYMENT_NOT_FOUND`，请验证提供商目标是上述网关端点，而不是 `https://api.vercel.ai`。

### Gemini 说明

- 提供商 ID：`gemini`（别名：`google`、`google-gemini`）
- 认证可以来自 `GEMINI_API_KEY`、`GOOGLE_API_KEY` 或 Gemini CLI OAuth 缓存（`~/.gemini/oauth_creds.json`）
- API 密钥请求使用 `generativelanguage.googleapis.com/v1beta`
- Gemini CLI OAuth 请求使用 `cloudcode-pa.googleapis.com/v1internal` 搭配代码辅助请求信封语义
- 支持思考模型（例如 `gemini-3-pro-preview`）—— 内部推理部分会自动从响应中过滤掉。

### Ollama 视觉说明

- 提供商 ID：`ollama`
- 通过用户消息图像标记支持视觉输入：``[IMAGE:<source>]``。
- 多模态归一化后，ZeroClaw 通过 Ollama 原生的 `messages[].images` 字段发送图像负载。
- 如果选择了不支持视觉的提供商，ZeroClaw 会返回结构化能力错误，而不是静默忽略图像。

### Ollama 云路由说明

- 仅在使用远程 Ollama 端点时使用 `:cloud` 模型后缀。
- 远程端点应在 `api_url` 中设置（例如：`https://ollama.com`）。
- ZeroClaw 会自动归一化 `api_url` 中末尾的 `/api`。
- 如果 `default_model` 以 `:cloud` 结尾，而 `api_url` 是本地的或未设置，配置验证会提前失败并返回可操作的错误。
- 本地 Ollama 模型发现会故意排除 `:cloud` 条目，以避免在本地模式下选择仅云端可用的模型。

### llama.cpp 服务器说明

- 提供商 ID：`llamacpp`（别名：`llama.cpp`）
- 默认端点：`http://localhost:8080/v1`
- 默认情况下 API 密钥是可选的；仅当 `llama-server` 使用 `--api-key` 启动时才需要设置 `LLAMACPP_API_KEY`。
- 模型发现：`zeroclaw models refresh --provider llamacpp`

### SGLang 服务器说明

- 提供商 ID：`sglang`
- 默认端点：`http://localhost:30000/v1`
- 默认情况下 API 密钥是可选的；仅当服务器需要认证时才设置 `SGLANG_API_KEY`。
- 工具调用需要使用 `--tool-call-parser` 启动 SGLang（例如 `hermes`、`llama3`、`qwen25`）。
- 模型发现：`zeroclaw models refresh --provider sglang`

### vLLM 服务器说明

- 提供商 ID：`vllm`
- 默认端点：`http://localhost:8000/v1`
- 默认情况下 API 密钥是可选的；仅当服务器需要认证时才设置 `VLLM_API_KEY`。
- 模型发现：`zeroclaw models refresh --provider vllm`

### Osaurus 服务器说明

- 提供商 ID：`osaurus`
- 默认端点：`http://localhost:1337/v1`
- API 密钥默认为 `"osaurus"` 但可选；设置 `OSAURUS_API_KEY` 覆盖或留空实现无密钥访问。
- 模型发现：`zeroclaw models refresh --provider osaurus`
- [Osaurus](https://github.com/dinoki-ai/osaurus) 是适用于 macOS（Apple Silicon）的统一 AI 边缘运行时，将本地 MLX 推理与云提供商代理通过单个端点结合。
- 同时支持多种 API 格式：兼容 OpenAI（`/v1/chat/completions`）、Anthropic（`/messages`）、Ollama（`/chat`）和开放响应（`/v1/responses`）。
- 内置 MCP（模型上下文协议）支持，用于工具和上下文服务器连接。
- 本地模型通过 MLX 运行（Llama、Qwen、Gemma、GLM、Phi、Nemotron 等）；云模型被透明代理。

### Bedrock 说明

- 提供商 ID：`bedrock`（别名：`aws-bedrock`）
- API：[Converse API](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html)
- 认证：AWS AKSK（不是单个 API 密钥）。设置 `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` 环境变量。
- 可选：`AWS_SESSION_TOKEN` 用于临时/STS 凭证，`AWS_REGION` 或 `AWS_DEFAULT_REGION`（默认：`us-east-1`）。
- 默认引导模型：`anthropic.claude-sonnet-4-5-20250929-v1:0`
- 支持原生工具调用和提示缓存（`cachePoint`）。
- 支持跨区域推理配置文件（例如 `us.anthropic.claude-*`）。
- 模型 ID 使用 Bedrock 格式：`anthropic.claude-sonnet-4-6`、`anthropic.claude-opus-4-6-v1` 等。

### Ollama 推理切换

你可以从 `config.toml` 控制 Ollama 推理/思考行为：

```toml
[runtime]
reasoning_enabled = false
```

行为：

- `false`：向 Ollama `/api/chat` 请求发送 `think: false`。
- `true`：发送 `think: true`。
- 未设置：省略 `think` 并保持 Ollama/模型默认值。

### Kimi Code 说明

- 提供商 ID：`kimi-code`
- 端点：`https://api.kimi.com/coding/v1`
- 默认引导模型：`kimi-for-coding`（替代：`kimi-k2.5`）
- 运行时自动添加 `User-Agent: KimiCLI/0.77` 以确保兼容性。

### NVIDIA NIM 说明

- 标准提供商 ID：`nvidia`
- 别名：`nvidia-nim`、`build.nvidia.com`
- 基础 API URL：`https://integrate.api.nvidia.com/v1`
- 模型发现：`zeroclaw models refresh --provider nvidia`

推荐的入门模型 ID（2026年2月18日针对 NVIDIA API 目录验证）：

- `meta/llama-3.3-70b-instruct`
- `deepseek-ai/deepseek-v3.2`
- `nvidia/llama-3.3-nemotron-super-49b-v1.5`
- `nvidia/llama-3.1-nemotron-ultra-253b-v1`

## 自定义端点

- 兼容 OpenAI 的端点：

```toml
default_provider = \"custom:https://your-api.example.com\"
```

- 兼容 Anthropic 的端点：

```toml
default_provider = \"anthropic-custom:https://your-api.example.com\"
```

## MiniMax OAuth 安装（config.toml）

在配置中设置 MiniMax 提供商和 OAuth 占位符：

```toml
default_provider = \"minimax-oauth\"
api_key = \"minimax-oauth\"
```

然后通过环境变量提供以下凭证之一：

- `MINIMAX_OAUTH_TOKEN`（首选，直接访问令牌）
- `MINIMAX_API_KEY`（旧版/静态令牌）
- `MINIMAX_OAUTH_REFRESH_TOKEN`（启动时自动刷新访问令牌）

可选：

- `MINIMAX_OAUTH_REGION=global` 或 `cn`（由提供商别名默认设置）
- `MINIMAX_OAUTH_CLIENT_ID` 覆盖默认 OAuth 客户端 ID

渠道兼容性说明：

- 对于 MiniMax 支持的渠道对话，运行时历史会被归一化以保持有效的 `user`/`assistant` 轮次顺序。
- 渠道特定的交付指导（例如 Telegram 附件标记）会合并到前置系统提示中，而不是作为末尾的 `system` 轮次追加。

## Qwen Code OAuth 安装（config.toml）

在配置中设置 Qwen Code OAuth 模式：

```toml
default_provider = \"qwen-code\"
api_key = \"qwen-oauth\"
```

`qwen-code` 的凭证解析：

1. 显式 `api_key` 值（如果不是占位符 `qwen-oauth`）
2. `QWEN_OAUTH_TOKEN`
3. `~/.qwen/oauth_creds.json`（复用 Qwen Code 缓存的 OAuth 凭证）
4. 通过 `QWEN_OAUTH_REFRESH_TOKEN`（或缓存的刷新令牌）可选刷新
5. 如果未使用 OAuth 占位符，`DASHSCOPE_API_KEY` 仍可用作回退

可选端点覆盖：

- `QWEN_OAUTH_RESOURCE_URL`（必要时归一化为 `https://.../v1`）
- 如果未设置，将使用缓存 OAuth 凭证中的 `resource_url`（如果可用）。

## 模型路由（`hint:<name>`）

你可以使用 `[[model_routes]]` 按提示路由模型调用：

```toml
[[model_routes]]
hint = \"reasoning\"
provider = \"openrouter\"
model = \"anthropic/claude-opus-4-20250514\"

[[model_routes]]
hint = \"fast\"
provider = \"groq\"
model = \"llama-3.3-70b-versatile\"
```

然后使用提示模型名称调用（例如从工具或集成路径）：

```text
hint:reasoning
```

## 嵌入路由（`hint:<name>`）

你可以使用 `[[embedding_routes]]` 以相同的提示模式路由嵌入调用。
将 `[memory].embedding_model` 设置为 `hint:<name>` 值以激活路由。

```toml
[memory]
embedding_model = \"hint:semantic\"

[[embedding_routes]]
hint = \"semantic\"
provider = \"openai\"
model = \"text-embedding-3-small\"
dimensions = 1536

[[embedding_routes]]
hint = \"archive\"
provider = \"custom:https://embed.example.com/v1\"
model = \"your-embedding-model-id\"
dimensions = 1024
```

支持的嵌入提供商：

- `none`
- `openai`
- `custom:<url>`（兼容 OpenAI 的嵌入端点）

可选的每条路由密钥覆盖：

```toml
[[embedding_routes]]
hint = \"semantic\"
provider = \"openai\"
model = \"text-embedding-3-small\"
api_key = \"sk-route-specific\"
```

## 安全升级模型

当提供商弃用模型 ID 时，使用稳定提示并仅更新路由目标。

推荐工作流：

1. 保持调用站点稳定（`hint:reasoning`、`hint:semantic`）。
2. 仅更改 `[[model_routes]]` 或 `[[embedding_routes]]` 下的目标模型。
3. 运行：
   - `zeroclaw doctor`
   - `zeroclaw status`
4. 在部署前冒烟测试一个代表性流程（聊天 + 内存检索）。

这最大程度减少了中断，因为模型 ID 升级时集成和提示不需要更改。
