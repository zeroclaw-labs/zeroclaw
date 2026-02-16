<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>零开销。零妥协。100% Rust。100% 通用。</strong><br>
  ⚡️ <strong>在 $10 的硬件上运行，甚至 <5MB RAM：比 OpenClaw 少占用 99% 内存，比 Mac mini 便宜 98%！</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
</p>

快速、小巧且完全自主的 AI 助手基础设施 —— 随处部署，随意替换。

```
~3.4MB 二进制文件 · <10ms 启动时间 · 1,017 个测试 · 22+ 提供商 · 8 个 trait · 一切皆可插拔
```

### ✨ 特性

- 🏎️ **超轻量级：** <5MB 内存占用 —— 比 OpenClaw 核心小 99%。
- 💰 **极低成本：** 高效且足以在 $10 的硬件上运行 —— 比 Mac mini 便宜 98%。
- ⚡ **闪电般快速：** 启动速度快 400 倍，<10ms 启动（即使在 0.6GHz 核心上也只需不到 1 秒）。
- 🌍 **真正的便携性：** 单个自包含的二进制文件，跨 ARM, x86 和 RISC-V 运行。

### 为什么团队选择 ZeroClaw

- **默认精简：** 小巧的 Rust 二进制文件，快速启动，低内存占用。
- **设计安全：** 配对机制，严格沙箱，显式允许列表，工作区隔离。
- **完全可替换：** 核心系统均为 trait（提供商，通道，工具，记忆，隧道）。
- **无锁定：** 支持 OpenAI兼容的提供商 + 可插拔的自定义端点。

## 性能基准快照 (ZeroClaw vs OpenClaw)

本地机器快速基准测试 (macOS arm64, 2026年2月) 针对 0.8GHz 边缘硬件进行了标准化。

|                            | OpenClaw       | NanoBot        | PicoClaw        | ZeroClaw 🦀      |
| -------------------------- | -------------- | -------------- | --------------- | ---------------- |
| **语言**                   | TypeScript     | Python         | Go              | **Rust**         |
| **内存 (RAM)**             | > 1GB          | > 100MB        | < 10MB          | **< 5MB**        |
| **启动时间 (0.8GHz 核心)** | > 500s         | > 30s          | < 1s            | **< 10ms**       |
| **二进制大小**             | ~28MB (分发版) | N/A (脚本)     | ~8MB            | **3.4 MB**       |
| **成本**                   | Mac Mini $599  | Linux SBC ~$50 | Linux Board $10 | **任何硬件 $10** |

> 注意：ZeroClaw 结果是在 release 构建版本中使用 `/usr/bin/time -l` 测量的。OpenClaw 需要 Node.js 运行时（~390MB 开销）。PicoClaw 和 ZeroClaw 是静态二进制文件。

<p align="center">
  <img src="zero-claw.jpeg" alt="ZeroClaw vs OpenClaw Comparison" width="800" />
</p>

在本地复现 ZeroClaw 数据：

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

## 快速开始

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release
cargo install --path . --force

# 快速设置（无需提示）
zeroclaw onboard --api-key sk-... --provider openrouter

# 或者交互式向导
zeroclaw onboard --interactive

# 或者仅快速修复通道/允许列表
zeroclaw onboard --channels-only

# 聊天
zeroclaw agent -m "Hello, ZeroClaw!"

# 交互模式
zeroclaw agent

# 启动网关（Webhook 服务器）
zeroclaw gateway                # 默认: 127.0.0.1:8080
zeroclaw gateway --port 0       # 随机端口（安全加固）

# 启动全自主运行时
zeroclaw daemon

# 检查状态
zeroclaw status

# 运行系统诊断
zeroclaw doctor

# 检查通道健康状况
zeroclaw channel doctor

# 获取集成设置详情
zeroclaw integrations info Telegram

# 管理后台服务
zeroclaw service install
zeroclaw service status

# 从 OpenClaw 迁移记忆（先安全预览）
zeroclaw migrate openclaw --dry-run
zeroclaw migrate openclaw
```

> **开发回退（无全局安装）：** 在命令前加上 `cargo run --release --`（例如：`cargo run --release -- status`）。
> **低内存开发板（例如 Raspberry Pi 3, 1GB RAM）：** 如果内核在编译期间杀死了 rustc，请运行 `CARGO_BUILD_JOBS=1 cargo build --release`。

## 架构

每个子系统都是一个 **trait** —— 通过配置更改实现替换，无需修改代码。

<p align="center">
  <img src="docs/architecture.svg" alt="ZeroClaw Architecture" width="900" />
</p>

| 子系统       | Trait            | 自带支持                                                                                                                                                           | 扩展                                                   |
| ------------ | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------ |
| **AI 模型**  | `Provider`       | 22+ 提供商 (OpenRouter, Anthropic, OpenAI, Ollama, Venice, Groq, Mistral, xAI, DeepSeek, Together, Fireworks, Perplexity, Cohere, Bedrock 等)                      | `custom:https://your-api.com` — 任何 OpenAI 兼容的 API |
| **通道**     | `Channel`        | CLI, Telegram, Discord, Slack, iMessage, Matrix, WhatsApp, Webhook                                                                                                 | 任何消息 API                                           |
| **记忆**     | `Memory`         | SQLite 混合搜索 (FTS5 + 向量余弦相似度), Markdown                                                                                                                  | 任何持久化后端                                         |
| **工具**     | `Tool`           | shell, file_read, file_write, memory_store, memory_recall, memory_forget, browser_open (Brave + allowlist), browser (agent-browser / rust-native), composio (可选) | 任何能力                                               |
| **可观测性** | `Observer`       | Noop, Log, Multi                                                                                                                                                   | Prometheus, OTel                                       |
| **运行时**   | `RuntimeAdapter` | Native, Docker (沙箱化)                                                                                                                                            | WASM (计划中；不支持的类型会快速失败)                  |
| **安全性**   | `SecurityPolicy` | 网关配对, 沙箱, 允许列表, 速率限制, 文件系统范围限制, 加密机密                                                                                                     | —                                                      |
| **身份**     | `IdentityConfig` | OpenClaw (markdown), AIEOS v1.1 (JSON)                                                                                                                             | 任何身份格式                                           |
| **隧道**     | `Tunnel`         | None, Cloudflare, Tailscale, ngrok, Custom                                                                                                                         | 任何隧道二进制文件                                     |
| **心跳**     | Engine           | HEARTBEAT.md 周期性任务                                                                                                                                            | —                                                      |
| **技能**     | Loader           | TOML 清单 + SKILL.md 说明                                                                                                                                          | 社区技能包                                             |
| **集成**     | Registry         | 9 个类别中的 50+ 个集成                                                                                                                                            | 插件系统                                               |

### 运行时支持 (当前)

- ✅ 目前支持：`runtime.kind = "native"` 或 `runtime.kind = "docker"`
- 🚧 计划中，尚未实现：WASM / 边缘运行时

当配置了不支持的 `runtime.kind` 时，ZeroClaw 现在会以明确的错误退出，而不是静默回退到原生模式。

### 记忆系统（全栈搜索引擎）

全自定义，零外部依赖 —— 无需 Pinecone，无需 Elasticsearch，无需 LangChain：

| 层级             | 实现                                                    |
| ---------------- | ------------------------------------------------------- |
| **向量数据库**   | 嵌入存储为 SQLite 中的 BLOB，余弦相似度搜索             |
| **关键词搜索**   | 带有 BM25 评分的 FTS5 虚拟表                            |
| **混合合并**     | 自定义加权合并函数 (`vector.rs`)                        |
| **嵌入**         | `EmbeddingProvider` trait — OpenAI, 自定义 URL, 或 noop |
| **分块**         | 基于行的 Markdown 分块器，带有标题保留                  |
| **缓存**         | SQLite `embedding_cache` 表，带有 LRU 驱逐              |
| **安全重建索引** | 原子重建 FTS5 + 重新嵌入缺失的向量                      |

代理通过工具自动回忆、保存和管理记忆。

```toml
[memory]
backend = "sqlite"          # "sqlite", "markdown", "none"
auto_save = true
embedding_provider = "openai"
vector_weight = 0.7
keyword_weight = 0.3
```

## 安全性

ZeroClaw 在**每一层**强制执行安全性 —— 不仅仅是沙箱。它通过了社区安全检查清单的所有项目。

### 安全检查清单

| #   | 项目                      | 状态 | 如何实现                                                                                                                                   |
| --- | ------------------------- | ---- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | **网关未公开暴露**        | ✅   | 默认绑定 `127.0.0.1`。如果没有隧道或显式的 `allow_public_bind = true`，拒绝 `0.0.0.0`。                                                    |
| 2   | **需要配对**              | ✅   | 启动时的 6 位一次性代码。通过 `POST /pair` 交换 bearer 令牌。所有 `/webhook` 请求都需要 `Authorization: Bearer <token>`。                  |
| 3   | **文件系统作用域 (无 /)** | ✅   | 默认 `workspace_only = true`。阻止 14 个系统目录 + 4 个敏感点文件。阻止空字节注入。通过规范化 + resolved-path 工作区检查防止符号链接逃逸。 |
| 4   | **仅通过隧道访问**        | ✅   | 网关在没有活动隧道的情况下拒绝公开绑定。支持 Tailscale, Cloudflare, ngrok 或任何自定义隧道。                                               |

> **运行你自己的 nmap：** `nmap -p 1-65535 <your-host>` — ZeroClaw 仅绑定到 localhost，因此除非你显式配置隧道，否则不会暴漏任何内容。

### 通道允许列表 (Telegram / Discord / Slack)

入站发送者策略现在是一致的：

- 空允许列表 = **拒绝所有入站消息**
- `"*"` = **允许所有** (显式加入)
- 否则 = 精确匹配允许列表

默认情况下，这可以保持较低的意外暴露风险。

推荐的低摩擦设置（安全 + 快速）：

- **Telegram:** 将你自己的 `@username`（不带 `@`）和/或你的数字 Telegram 用户 ID 加入允许列表。
- **Discord:** 将你自己的 Discord 用户 ID 加入允许列表。
- **Slack:** 将你自己的 Slack 成员 ID（通常以 `U` 开头）加入允许列表。
- 仅在临时开放测试时使用 `"*"`。

如果你不确定使用哪个身份：

1. 启动通道并向你的机器人发送一条消息。
2. 阅读警告日志以查看确切的发件人身份。
3. 将该值添加到允许列表并重新运行仅通道设置。

如果你在日志中遇到授权警告（例如：`ignoring message from unauthorized user`），
仅重新运行通道设置：

```bash
zeroclaw onboard --channels-only
```

### WhatsApp Business Cloud API 设置

WhatsApp 使用 Meta 的 Cloud API 和 webhook（基于推送，而不是轮询）：

1. **创建 Meta Business App:**
   - 前往 [developers.facebook.com](https://developers.facebook.com)
   - 创建新应用 → 选择 "Business" 类型
   - 添加 "WhatsApp" 产品

2. **获取你的凭据:**
   - **Access Token:** 从 WhatsApp → API Setup → Generate token (或创建一个系统用户以获取永久令牌)
   - **Phone Number ID:** 从 WhatsApp → API Setup → Phone number ID
   - **Verify Token:** 你定义这个（任何随机字符串）— Meta 将在 webhook 验证期间发回它

3. **配置 ZeroClaw:**

   ```toml
   [channels_config.whatsapp]
   access_token = "EAABx..."
   phone_number_id = "123456789012345"
   verify_token = "my-secret-verify-token"
   allowed_numbers = ["+1234567890"]  # E.164 格式, 或 ["*"] 代表所有
   ```

4. **使用隧道启动网关:**

   ```bash
   zeroclaw gateway --port 8080
   ```

   WhatsApp 需要 HTTPS，所以使用隧道 (ngrok, Cloudflare, Tailscale Funnel)。

5. **配置 Meta webhook:**
   - 在 Meta 开发者控制台 → WhatsApp → Configuration → Webhook
   - **Callback URL:** `https://your-tunnel-url/whatsapp`
   - **Verify Token:** 与配置中的 `verify_token` 相同
   - 订阅 `messages` 字段

6. **测试:** 向你的 WhatsApp Business 号码发送消息 — ZeroClaw 将通过 LLM 回复。

## 配置

配置：`~/.zeroclaw/config.toml` (由 `onboard` 创建)

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-20250514"
default_temperature = 0.7

[memory]
backend = "sqlite"              # "sqlite", "markdown", "none"
auto_save = true
embedding_provider = "openai"   # "openai", "noop"
vector_weight = 0.7
keyword_weight = 0.3

[gateway]
require_pairing = true          # 首次连接需要配对码
allow_public_bind = false       # 无隧道拒绝 0.0.0.0

[autonomy]
level = "supervised"            # "readonly", "supervised", "full" (默认: supervised)
workspace_only = true           # 默认: true — 仅限工作区
allowed_commands = ["git", "npm", "cargo", "ls", "cat", "grep"]
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]

[runtime]
kind = "native"                # "native" 或 "docker"

[runtime.docker]
image = "alpine:3.20"          # shell 执行的容器镜像
network = "none"               # docker 网络模式 ("none", "bridge", 等)
memory_limit_mb = 512          # 可选内存限制 MB
cpu_limit = 1.0                # 可选 CPU 限制
read_only_rootfs = true        # 以只读方式挂载根文件系统
mount_workspace = true         # 将工作区挂载到 /workspace
allowed_workspace_roots = []   # 工作区挂载验证的可选允许列表

[heartbeat]
enabled = false
interval_minutes = 30

[tunnel]
provider = "none"               # "none", "cloudflare", "tailscale", "ngrok", "custom"

[secrets]
encrypt = true                  # API 密钥使用本地密钥文件加密

[browser]
enabled = false                        # 开启 browser_open + browser 工具
allowed_domains = ["docs.rs"]         # 启用浏览器时必须
backend = "agent_browser"             # "agent_browser" (默认), "rust_native", "auto"
native_headless = true                 # 当后端使用 rust-native 时适用
native_webdriver_url = "http://127.0.0.1:9515" # WebDriver端点 (chromedriver/selenium)
# native_chrome_path = "/usr/bin/chromium"  # 驱动程序的可选显式浏览器二进制文件

# Rust-native 后端构建标志:
# cargo build --release --features browser-native
# 确保 WebDriver 服务器正在运行，例如 chromedriver --port=9515

[composio]
enabled = false                 # 开启: 1000+ OAuth 应用通过 composio.dev
# api_key = "cmp_..."          # 可选: 当 [secrets].encrypt = true 时加密存储
entity_id = "default"         # Composio 工具调用的默认 user_id

[identity]
format = "openclaw"             # "openclaw" (默认, markdown 文件) 或 "aieos" (JSON)
# aieos_path = "identity.json"  # AIEOS JSON 文件路径 (相对工作区或是绝对路径)
# aieos_inline = '{"identity":{"names":{"first":"Nova"}}}'  # 内联 AIEOS JSON
```

## 身份系统 (支持 AIEOS)

ZeroClaw 通过两种格式支持**身份无关**的 AI 人格：

### OpenClaw (默认)

工作区中的传统 markdown 文件：

- `IDENTITY.md` — 代理是谁
- `SOUL.md` — 核心个性和价值观
- `USER.md` — 代理正在帮助谁
- `AGENTS.md` — 行为准则

### AIEOS (AI 实体对象规范)

[AIEOS](https://aieos.org) 是便携式 AI 身份的标准化框架。ZeroClaw 支持 AIEOS v1.1 JSON 负载，允许你：

- 从 AIEOS 生态系统**导入身份**
- **导出身份**到其他兼容 AIEOS 的系统
- 在不同的 AI 模型之间**保持行为完整性**

#### 启用 AIEOS

```toml
[identity]
format = "aieos"
aieos_path = "identity.json"  # 相对工作区或是绝对路径
```

或者内联 JSON：

```toml
[identity]
format = "aieos"
aieos_inline = '''
{
  "identity": {
    "names": { "first": "Nova", "nickname": "N" }
  },
  "psychology": {
    "neural_matrix": { "creativity": 0.9, "logic": 0.8 },
    "traits": { "mbti": "ENTP" },
    "moral_compass": { "alignment": "Chaotic Good" }
  },
  "linguistics": {
    "text_style": { "formality_level": 0.2, "slang_usage": true }
  },
  "motivations": {
    "core_drive": "Push boundaries and explore possibilities"
  }
}
'''
```

#### AIEOS Schema 章节

| 章节           | 描述                                         |
| -------------- | -------------------------------------------- |
| `identity`     | 姓名, 简介, 起源, 居住地                     |
| `psychology`   | 神经矩阵 (认知权重), MBTI, OCEAN, 道德指南针 |
| `linguistics`  | 文本风格, 正式程度, 口头禅, 禁用词           |
| `motivations`  | 核心驱动力, 短期/长期目标, 恐惧              |
| `capabilities` | 代理可以访问的技能和工具                     |
| `physicality`  | 图像生成的视觉描述符                         |
| `history`      | 起源故事, 教育, 职业                         |
| `interests`    | 爱好, 收藏, 生活方式                         |

查看 [aieos.org](https://aieos.org) 获取完整模式和实时示例。

## 网关 API

| 端点        | 方法 | 认证                            | 描述                                                          |
| ----------- | ---- | ------------------------------- | ------------------------------------------------------------- |
| `/health`   | GET  | None                            | 健康检查（总是公开，不泄露机密）                              |
| `/pair`     | POST | `X-Pairing-Code` header         | 用一次性代码换取 bearer 令牌                                  |
| `/webhook`  | POST | `Authorization: Bearer <token>` | 发送消息: `{"message": "your prompt"}`                        |
| `/whatsapp` | GET  | Query params                    | Meta webhook 验证 (hub.mode, hub.verify_token, hub.challenge) |
| `/whatsapp` | POST | None (Meta signature)           | WhatsApp 传入消息 webhook                                     |

## 命令

| 命令                                          | 描述                                         |
| --------------------------------------------- | -------------------------------------------- |
| `onboard`                                     | 快速设置 (默认)                              |
| `onboard --interactive`                       | 完整的交互式 7 步向导                        |
| `onboard --channels-only`                     | 仅重新配置通道/允许列表（快速修复流程）      |
| `agent -m "..."`                              | 单条消息模式                                 |
| `agent`                                       | 交互式聊天模式                               |
| `gateway`                                     | 启动 webhook 服务器 (默认: `127.0.0.1:8080`) |
| `gateway --port 0`                            | 随机端口模式                                 |
| `daemon`                                      | 启动长期运行的自主运行时                     |
| `service install/start/stop/status/uninstall` | 管理用户级后台服务                           |
| `doctor`                                      | 诊断 daemon/scheduler/channel 新鲜度         |
| `status`                                      | 显示完整系统状态                             |
| `channel doctor`                              | 运行已配置通道的健康检查                     |
| `integrations info <name>`                    | 显示一个集成的设置/状态详情                  |

## 开发

```bash
cargo build              # 开发构建
cargo build --release    # Release 构建 (~3.4MB)
CARGO_BUILD_JOBS=1 cargo build --release    # 低内存回退 (Raspberry Pi 3, 1GB RAM)
cargo test               # 1,017 个测试
cargo clippy             # Lint (0 警告)
cargo fmt                # 格式化

# 运行 SQLite vs Markdown 基准测试
cargo test --test memory_comparison -- --nocapture
```

### 推送前钩子 (Pre-push hook)

git hook 会在每次推送前运行 `cargo fmt --check`, `cargo clippy -- -D warnings`, 和 `cargo test`。启用一次：

```bash
git config core.hooksPath .githooks
```

如果需要在开发过程中快速推送并跳过钩子：

```bash
git push --no-verify
```

## 协作与文档

为了高效协作和一致的审查：

- 贡献指南: [CONTRIBUTING.md](CONTRIBUTING.md)
- PR 工作流策略: [docs/pr-workflow.md](docs/pr-workflow.md)
- 审查员手册 (分流 + 深度审查): [docs/reviewer-playbook.md](docs/reviewer-playbook.md)
- CI 所有权和分流图: [docs/ci-map.md](docs/ci-map.md)
- 安全披露策略: [SECURITY.md](SECURITY.md)

## 支持

ZeroClaw 是一个充满激情的开源项目。如果你觉得它有用，并希望支持其持续开发、测试硬件以及维护者的咖啡，你可以在这里支持我：

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

## 许可证

MIT — 参见 [LICENSE](LICENSE)

## 贡献

参见 [CONTRIBUTING.md](CONTRIBUTING.md)。实现一个 trait，提交 PR：

- CI 工作流指南: [docs/ci-map.md](docs/ci-map.md)
- 新 `Provider` → `src/providers/`
- 新 `Channel` → `src/channels/`
- 新 `Observer` → `src/observability/`
- 新 `Tool` → `src/tools/`
- 新 `Memory` → `src/memory/`
- 新 `Tunnel` → `src/tunnel/`
- 新 `Skill` → `~/.zeroclaw/workspace/skills/<name>/`

---

**ZeroClaw** — 零开销。零妥协。随处部署。随意替换。 🦀

## Star 历史

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&Date">
    <img src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=Date" alt="Star History Chart" />
  </a>
</p>
