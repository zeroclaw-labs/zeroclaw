<p align="center">
  <img src="https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/docs/assets/zeroclaw-banner.png" alt="ZeroClaw" width="600" />
</p>

<h1 align="center">🦀 ZeroClaw — 个人AI助手</h1>

<p align="center">
  <strong>零开销。零妥协。100% Rust。100% 无绑定。</strong><br>
  ⚡️ <strong>在10美元硬件上运行，RAM不到5MB：比OpenClaw少99%内存，比Mac mini便宜98%！</strong>
</p>

<p align="center">
  <a href="https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/ci-run.yml"><img src="https://img.shields.io/github/actions/workflow/status/zeroclaw-labs/zeroclaw/ci-run.yml?branch=master&label=build" alt="Build Status" /></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-edition%202024-orange?logo=rust" alt="Rust Edition 2024" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/releases/latest"><img src="https://img.shields.io/badge/version-v0.6.7-blue" alt="Version v0.6.7" /></a>
  <a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Contributors" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://discord.com/invite/wDshRVqRjx"><img src="https://img.shields.io/badge/Discord-Join-5865F2?style=flat&logo=discord&logoColor=white" alt="Discord" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
由哈佛大学、麻省理工学院和 Sundai.Club 社区的学生及成员构建。
</p>

<p align="center">
  🌐 <strong>Languages:</strong>
  <a href="../../../README.md">🇺🇸 English</a> ·
  <a href="../zh-CN/README.md">🇨🇳 简体中文</a> ·
  <a href="../ja/README.md">🇯🇵 日本語</a> ·
  <a href="../ko/README.md">🇰🇷 한국어</a> ·
  <a href="../vi/README.md">🇻🇳 Tiếng Việt</a> ·
  <a href="../tl/README.md">🇵🇭 Tagalog</a> ·
  <a href="../es/README.md">🇪🇸 Español</a> ·
  <a href="../pt/README.md">🇧🇷 Português</a> ·
  <a href="../it/README.md">🇮🇹 Italiano</a> ·
  <a href="../de/README.md">🇩🇪 Deutsch</a> ·
  <a href="../fr/README.md">🇫🇷 Français</a> ·
  <a href="../ar/README.md">🇸🇦 العربية</a> ·
  <a href="../hi/README.md">🇮🇳 हिन्दी</a> ·
  <a href="../ru/README.md">🇷🇺 Русский</a> ·
  <a href="../bn/README.md">🇧🇩 বাংলা</a> ·
  <a href="../he/README.md">🇮🇱 עברית</a> ·
  <a href="../pl/README.md">🇵🇱 Polski</a> ·
  <a href="../cs/README.md">🇨🇿 Čeština</a> ·
  <a href="../nl/README.md">🇳🇱 Nederlands</a> ·
  <a href="../tr/README.md">🇹🇷 Türkçe</a> ·
  <a href="../uk/README.md">🇺🇦 Українська</a> ·
  <a href="../id/README.md">🇮🇩 Bahasa Indonesia</a> ·
  <a href="../th/README.md">🇹🇭 ไทย</a> ·
  <a href="../ur/README.md">🇵🇰 اردو</a> ·
  <a href="../ro/README.md">🇷🇴 Română</a> ·
  <a href="../sv/README.md">🇸🇪 Svenska</a> ·
  <a href="../el/README.md">🇬🇷 Ελληνικά</a> ·
  <a href="../hu/README.md">🇭🇺 Magyar</a> ·
  <a href="../fi/README.md">🇫🇮 Suomi</a> ·
  <a href="../da/README.md">🇩🇰 Dansk</a> ·
  <a href="../nb/README.md">🇳🇴 Norsk</a>
</p>

ZeroClaw 是一个运行在你自己设备上的个人AI助手。它在你已经使用的频道上回复你（WhatsApp、Telegram、Slack、Discord、Signal、iMessage、Matrix、IRC、Email、Bluesky、Nostr、Mattermost、Nextcloud Talk、DingTalk、Lark、QQ、Reddit、LinkedIn、Twitter、MQTT、WeChat Work 等）。它有一个用于实时控制的网页仪表板，可以连接硬件外设（ESP32、STM32、Arduino、Raspberry Pi）。Gateway 只是控制平面——产品是助手本身。

如果你想要一个本地化、快速、始终在线的个人单用户助手，这就是它。

<p align="center">
  <a href="https://www.zeroclawlabs.ai">官网</a> ·
  <a href="docs/README.md">文档</a> ·
  <a href="docs/architecture.md">架构</a> ·
  <a href="#快速开始简版">入门指南</a> ·
  <a href="#从-openclaw-迁移">从 OpenClaw 迁移</a> ·
  <a href="docs/ops/troubleshooting.md">故障排除</a> ·
  <a href="https://discord.com/invite/wDshRVqRjx">Discord</a>
</p>

> **推荐设置方式：** 在终端运行 `zeroclaw onboard`。ZeroClaw Onboard 会引导你逐步设置网关、工作区、频道和提供者。这是推荐的设置路径，支持 macOS、Linux 和 Windows（通过 WSL2）。首次安装？从这里开始：[入门指南](#快速开始简版)

### 订阅认证（OAuth）

- **OpenAI Codex**（ChatGPT 订阅）
- **Gemini**（Google OAuth）
- **Anthropic**（API 密钥或认证令牌）

模型说明：虽然支持许多提供者/模型，但为获得最佳体验，请使用你可用的最强最新一代模型。参见[引导设置](#快速开始简版)。

模型配置 + CLI：[提供者参考](docs/reference/api/providers-reference.md)
认证配置轮换（OAuth 与 API 密钥）+ 故障转移：[模型故障转移](docs/reference/api/providers-reference.md)

## 安装（推荐）

运行时：Rust stable 工具链。单一二进制文件，无运行时依赖。

### Homebrew（macOS/Linuxbrew）

```bash
brew install zeroclaw
```

### 一键安装

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

`zeroclaw onboard` 在安装后自动运行，配置你的工作区和提供者。

## 快速开始（简版）

完整新手指南（认证、配对、频道）：[入门指南](docs/setup-guides/one-click-bootstrap.md)

```bash
# 安装 + 引导
./install.sh --api-key "sk-..." --provider openrouter

# 启动网关（webhook 服务器 + 网页仪表板）
zeroclaw gateway                # 默认：127.0.0.1:42617
zeroclaw gateway --port 0       # 随机端口（安全加固）

# 与助手对话
zeroclaw agent -m "Hello, ZeroClaw!"

# 交互模式
zeroclaw agent

# 启动完整自主运行时（网关 + 频道 + 定时任务 + 手）
zeroclaw daemon

# 检查状态
zeroclaw status

# 运行诊断
zeroclaw doctor
```

升级？更新后运行 `zeroclaw doctor`。

### 从源码构建（开发）

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

cargo build --release --locked
cargo install --path . --force --locked

zeroclaw onboard
```

> **开发替代方案（无全局安装）：** 命令前加 `cargo run --release --`（示例：`cargo run --release -- status`）。

## 从 OpenClaw 迁移

ZeroClaw 可以导入你的 OpenClaw 工作区、记忆和配置：

```bash
# 预览将迁移的内容（安全，只读）
zeroclaw migrate openclaw --dry-run

# 执行迁移
zeroclaw migrate openclaw
```

这会将你的记忆条目、工作区文件和配置从 `~/.openclaw/` 迁移到 `~/.zeroclaw/`。配置会自动从 JSON 转换为 TOML。

## 安全默认设置（DM 访问）

ZeroClaw 连接到真实的消息平台。将入站 DM 视为不可信输入。

完整安全指南：[SECURITY.md](SECURITY.md)

所有频道的默认行为：

- **DM 配对**（默认）：未知发送者会收到一个短配对码，机器人不会处理他们的消息。
- 使用以下命令批准：`zeroclaw pairing approve <channel> <code>`（然后发送者会被添加到本地允许列表）。
- 公共入站 DM 需要在 `config.toml` 中显式启用。
- 运行 `zeroclaw doctor` 来检测有风险或配置错误的 DM 策略。

**自主级别：**

| 级别 | 行为 |
|------|------|
| `ReadOnly` | 代理可以观察但不能操作 |
| `Supervised`（默认） | 代理在中/高风险操作时需要批准 |
| `Full` | 代理在策略范围内自主操作 |

**沙箱层：** 工作区隔离、路径遍历阻止、命令允许列表、禁止路径（`/etc`、`/root`、`~/.ssh`）、速率限制（每小时最大操作数、每日成本上限）。

<!-- BEGIN:WHATS_NEW -->
<!-- END:WHATS_NEW -->

### 📢 公告

使用此面板发布重要通知（破坏性更改、安全公告、维护窗口和发布阻塞问题）。

| 日期 (UTC) | 级别 | 通知 | 操作 |
| ---------- | ---- | ---- | ---- |
| 2026-02-19 | _严重_ | 我们与 `openagen/zeroclaw`、`zeroclaw.org` 或 `zeroclaw.net` **无任何关联**。`zeroclaw.org` 和 `zeroclaw.net` 域名目前指向 `openagen/zeroclaw` 分支，该域名/仓库正在冒充我们的官方网站/项目。 | 不要信任来自这些来源的信息、二进制文件、筹款或公告。仅使用[本仓库](https://github.com/zeroclaw-labs/zeroclaw)和我们经过验证的社交账号。 |
| 2026-02-19 | _重要_ | Anthropic 于 2026-02-19 更新了认证和凭证使用条款。Claude Code OAuth 令牌（Free、Pro、Max）仅供 Claude Code 和 Claude.ai 专用；在任何其他产品、工具或服务（包括 Agent SDK）中使用 Claude Free/Pro/Max 的 OAuth 令牌是不允许的，可能违反消费者服务条款。 | 请暂时避免 Claude Code OAuth 集成以防止潜在损失。原始条款：[Authentication and Credential Use](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)。 |

## 亮点

- **默认精简运行时** — 常见 CLI 和状态工作流在发布构建中运行仅需数兆字节内存。
- **低成本部署** — 专为 10 美元开发板和小型云实例设计，无重量级运行时依赖。
- **快速冷启动** — 单一二进制 Rust 运行时使命令和守护进程启动近乎即时。
- **可移植架构** — 跨 ARM、x86 和 RISC-V 的单一二进制文件，可交换的提供者/频道/工具。
- **本地优先网关** — 用于会话、频道、工具、定时任务、SOP 和事件的单一控制平面。
- **多频道收件箱** — WhatsApp、Telegram、Slack、Discord、Signal、iMessage、Matrix、IRC、Email、Bluesky、Nostr、Mattermost、Nextcloud Talk、DingTalk、Lark、QQ、Reddit、LinkedIn、Twitter、MQTT、WeChat Work、WebSocket 等。
- **多代理编排（Hands）** — 按计划运行并随时间变得更智能的自主代理群。
- **标准操作规程（SOPs）** — 事件驱动的工作流自动化，支持 MQTT、webhook、cron 和外设触发器。
- **网页仪表板** — React 19 + Vite 网页 UI，具有实时聊天、记忆浏览器、配置编辑器、定时任务管理器和工具检查器。
- **硬件外设** — 通过 `Peripheral` trait 支持 ESP32、STM32 Nucleo、Arduino、Raspberry Pi GPIO。
- **一流工具** — shell、文件 I/O、浏览器、git、网页抓取/搜索、MCP、Jira、Notion、Google Workspace 等 70+ 种。
- **生命周期钩子** — 在每个阶段拦截和修改 LLM 调用、工具执行和消息。
- **技能平台** — 内置、社区和工作区技能，带安全审计。
- **隧道支持** — Cloudflare、Tailscale、ngrok、OpenVPN 和自定义隧道用于远程访问。

### 团队为什么选择 ZeroClaw

- **默认精简：** 小型 Rust 二进制文件，快速启动，低内存占用。
- **安全设计：** 配对、严格沙箱、显式允许列表、工作区范围限定。
- **完全可替换：** 核心系统都是 trait（提供者、频道、工具、记忆、隧道）。
- **无锁定：** 支持 OpenAI 兼容提供者 + 可插拔自定义端点。

## 基准测试快照（ZeroClaw 对比 OpenClaw，可复现）

本地机器快速基准测试（macOS arm64，2026年2月），针对 0.8GHz 边缘硬件标准化。

|                           | OpenClaw      | NanoBot        | PicoClaw        | ZeroClaw 🦀          |
| ------------------------- | ------------- | -------------- | --------------- | -------------------- |
| **语言**                  | TypeScript    | Python         | Go              | **Rust**             |
| **RAM**                   | > 1GB         | > 100MB        | < 10MB          | **< 5MB**            |
| **启动时间（0.8GHz 核心）** | > 500s        | > 30s          | < 1s            | **< 10ms**           |
| **二进制大小**            | ~28MB (dist)  | N/A (Scripts)  | ~8MB            | **~8.8 MB**          |
| **成本**                  | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **任何硬件 $10**     |

> 注意：ZeroClaw 的结果使用 `/usr/bin/time -l` 在发布构建上测量。OpenClaw 需要 Node.js 运行时（通常约 390MB 额外内存开销），而 NanoBot 需要 Python 运行时。PicoClaw 和 ZeroClaw 是静态二进制文件。上述 RAM 数据为运行时内存；构建时编译需求更高。

<p align="center">
  <img src="docs/assets/zeroclaw-comparison.jpeg" alt="ZeroClaw vs OpenClaw Comparison" width="800" />
</p>

### 可复现的本地测量

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

## 我们迄今为止构建的一切

### 核心平台

- Gateway HTTP/WS/SSE 控制平面，支持会话、在线状态、配置、定时任务、webhook、网页仪表板和配对。
- CLI 表面：`gateway`、`agent`、`onboard`、`doctor`、`status`、`service`、`migrate`、`auth`、`cron`、`channel`、`skills`。
- 代理编排循环，支持工具调度、提示构建、消息分类和记忆加载。
- 会话模型，支持安全策略执行、自主级别和批准门控。
- 弹性提供者包装器，支持故障转移、重试和跨 20+ LLM 后端的模型路由。

### 频道

频道：WhatsApp（原生）、Telegram、Slack、Discord、Signal、iMessage、Matrix、IRC、Email、Bluesky、DingTalk、Lark、Mattermost、Nextcloud Talk、Nostr、QQ、Reddit、LinkedIn、Twitter、MQTT、WeChat Work、WATI、Mochat、Linq、Notion、WebSocket、ClawdTalk。

功能门控：Matrix（`channel-matrix`）、Lark（`channel-lark`）、Nostr（`channel-nostr`）。

### 网页仪表板

React 19 + Vite 6 + Tailwind CSS 4 网页仪表板直接从 Gateway 提供：

- **仪表板** — 系统概览、健康状态、运行时间、成本跟踪
- **代理聊天** — 与代理的交互式聊天
- **记忆** — 浏览和管理记忆条目
- **配置** — 查看和编辑配置
- **定时任务** — 管理计划任务
- **工具** — 浏览可用工具
- **日志** — 查看代理活动日志
- **成本** — 令牌使用和成本跟踪
- **诊断** — 系统健康诊断
- **集成** — 集成状态和设置
- **配对** — 设备配对管理

### 固件目标

| 目标 | 平台 | 用途 |
|------|------|------|
| ESP32 | Espressif ESP32 | 无线外设代理 |
| ESP32-UI | ESP32 + Display | 带可视化界面的代理 |
| STM32 Nucleo | STM32 (ARM Cortex-M) | 工业外设 |
| Arduino | Arduino | 基础传感器/执行器桥接 |
| Uno Q Bridge | Arduino Uno | 到代理的串口桥接 |

### 工具 + 自动化

- **核心：** shell、文件读/写/编辑、git 操作、glob 搜索、内容搜索
- **网络：** 浏览器控制、网页抓取、网络搜索、截图、图片信息、PDF 阅读
- **集成：** Jira、Notion、Google Workspace、Microsoft 365、LinkedIn、Composio、Pushover
- **MCP：** Model Context Protocol 工具包装器 + 延迟工具集
- **调度：** cron 添加/删除/更新/运行、计划工具
- **记忆：** 回忆、存储、遗忘、知识、项目情报
- **高级：** 委托（代理到代理）、群体、模型切换/路由、安全操作、云操作
- **硬件：** 板信息、内存映射、内存读取（功能门控）

### 运行时 + 安全

- **自主级别：** ReadOnly、Supervised（默认）、Full。
- **沙箱：** 工作区隔离、路径遍历阻止、命令允许列表、禁止路径、Landlock（Linux）、Bubblewrap。
- **速率限制：** 每小时最大操作数、每日最大成本（可配置）。
- **批准门控：** 中/高风险操作的交互式批准。
- **紧急停止：** 紧急关闭功能。
- **129+ 安全测试** 在自动化 CI 中。

### 运维 + 打包

- 网页仪表板直接从 Gateway 提供。
- 隧道支持：Cloudflare、Tailscale、ngrok、OpenVPN、自定义命令。
- Docker 运行时适配器用于容器化执行。
- CI/CD：beta（推送时自动）→ stable（手动触发）→ Docker、crates.io、Scoop、AUR、Homebrew、tweet。
- 预构建二进制文件支持 Linux（x86_64、aarch64、armv7）、macOS（x86_64、aarch64）、Windows（x86_64）。


## 配置

最小 `~/.zeroclaw/config.toml`：

```toml
default_provider = "anthropic"
api_key = "sk-ant-..."
```

完整配置参考：[docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)。

### 频道配置

**Telegram：**
```toml
[channels.telegram]
bot_token = "123456:ABC-DEF..."
```

**Discord：**
```toml
[channels.discord]
token = "your-bot-token"
```

**Slack：**
```toml
[channels.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."
```

**WhatsApp：**
```toml
[channels.whatsapp]
enabled = true
```

**Matrix：**
```toml
[channels.matrix]
homeserver_url = "https://matrix.org"
username = "@bot:matrix.org"
password = "..."
```

**Signal：**
```toml
[channels.signal]
phone_number = "+1234567890"
```

### 隧道配置

```toml
[tunnel]
kind = "cloudflare"  # or "tailscale", "ngrok", "openvpn", "custom", "none"
```

详情：[频道参考](docs/reference/api/channels-reference.md) · [配置参考](docs/reference/api/config-reference.md)

### 运行时支持（当前）

- **`native`**（默认）— 直接进程执行，最快路径，适合可信环境。
- **`docker`** — 完全容器隔离，强制安全策略，需要 Docker。

设置 `runtime.kind = "docker"` 以获得严格沙箱或网络隔离。

## 订阅认证（OpenAI Codex / Claude Code / Gemini）

ZeroClaw 支持订阅原生认证配置文件（多账户，静态加密）。

- 存储文件：`~/.zeroclaw/auth-profiles.json`
- 加密密钥：`~/.zeroclaw/.secret_key`
- 配置文件 ID 格式：`<provider>:<profile_name>`（示例：`openai-codex:work`）

```bash
# OpenAI Codex OAuth（ChatGPT 订阅）
zeroclaw auth login --provider openai-codex --device-code

# Gemini OAuth
zeroclaw auth login --provider gemini --profile default

# Anthropic setup-token
zeroclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# 检查 / 刷新 / 切换配置文件
zeroclaw auth status
zeroclaw auth refresh --provider openai-codex --profile default
zeroclaw auth use --provider openai-codex --profile work

# 使用订阅认证运行代理
zeroclaw agent --provider openai-codex -m "hello"
zeroclaw agent --provider anthropic -m "hello"
```

## 代理工作区 + 技能

工作区根目录：`~/.zeroclaw/workspace/`（可通过配置自定义）。

注入的提示文件：
- `IDENTITY.md` — 代理人格和角色
- `USER.md` — 用户上下文和偏好
- `MEMORY.md` — 长期事实和经验
- `AGENTS.md` — 会话约定和初始化规则
- `SOUL.md` — 核心身份和运作原则

技能：`~/.zeroclaw/workspace/skills/<skill>/SKILL.md` 或 `SKILL.toml`。

```bash
# 列出已安装的技能
zeroclaw skills list

# 从 git 安装
zeroclaw skills install https://github.com/user/my-skill.git

# 安装前安全审计
zeroclaw skills audit https://github.com/user/my-skill.git

# 移除技能
zeroclaw skills remove my-skill
```

## CLI 命令

```bash
# 工作区管理
zeroclaw onboard              # 引导设置向导
zeroclaw status               # 显示守护进程/代理状态
zeroclaw doctor               # 运行系统诊断

# 网关 + 守护进程
zeroclaw gateway              # 启动网关服务器（127.0.0.1:42617）
zeroclaw daemon               # 启动完整自主运行时

# 代理
zeroclaw agent                # 交互式聊天模式
zeroclaw agent -m "message"   # 单条消息模式

# 服务管理
zeroclaw service install      # 作为系统服务安装（launchd/systemd）
zeroclaw service start|stop|restart|status

# 频道
zeroclaw channel list         # 列出已配置的频道
zeroclaw channel doctor       # 检查频道健康状况
zeroclaw channel bind-telegram 123456789

# 定时任务 + 调度
zeroclaw cron list            # 列出计划任务
zeroclaw cron add "*/5 * * * *" --prompt "Check system health"
zeroclaw cron remove <id>

# 记忆
zeroclaw memory list          # 列出记忆条目
zeroclaw memory get <key>     # 检索记忆
zeroclaw memory stats         # 记忆统计

# 认证配置文件
zeroclaw auth login --provider <name>
zeroclaw auth status
zeroclaw auth use --provider <name> --profile <profile>

# 硬件外设
zeroclaw hardware discover    # 扫描已连接的设备
zeroclaw peripheral list      # 列出已连接的外设
zeroclaw peripheral flash     # 向设备刷写固件

# 迁移
zeroclaw migrate openclaw --dry-run
zeroclaw migrate openclaw

# Shell 补全
source <(zeroclaw completions bash)
zeroclaw completions zsh > ~/.zfunc/_zeroclaw
```

完整命令参考：[docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)

<!-- markdownlint-disable MD001 MD024 -->

## 前置条件

<details>
<summary><strong>Windows</strong></summary>

#### 必需

1. **Visual Studio Build Tools**（提供 MSVC 链接器和 Windows SDK）：

    ```powershell
    winget install Microsoft.VisualStudio.2022.BuildTools
    ```

    在安装期间（或通过 Visual Studio 安装程序），选择 **"Desktop development with C++"** 工作负载。

2. **Rust 工具链：**

    ```powershell
    winget install Rustlang.Rustup
    ```

    安装后，打开新终端并运行 `rustup default stable` 确保 stable 工具链已激活。

3. **验证**两者是否正常工作：
    ```powershell
    rustc --version
    cargo --version
    ```

#### 可选

- **Docker Desktop** — 仅在使用 [Docker 沙箱运行时](#运行时支持当前)（`runtime.kind = "docker"`）时需要。通过 `winget install Docker.DockerDesktop` 安装。

</details>

<details>
<summary><strong>Linux / macOS</strong></summary>

#### 必需

1. **构建工具：**
    - **Linux (Debian/Ubuntu):** `sudo apt install build-essential pkg-config`
    - **Linux (Fedora/RHEL):** `sudo dnf group install development-tools && sudo dnf install pkg-config`
    - **macOS:** 安装 Xcode 命令行工具：`xcode-select --install`

2. **Rust 工具链：**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

    详情参见 [rustup.rs](https://rustup.rs)。

3. **验证**两者是否正常工作：
    ```bash
    rustc --version
    cargo --version
    ```

#### 一行安装

或者跳过上述步骤，使用单条命令安装所有内容（系统依赖、Rust、ZeroClaw）：

```bash
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

#### 编译资源需求

从源码构建比运行生成的二进制文件需要更多资源：

| 资源 | 最低 | 推荐 |
| ---- | ---- | ---- |
| **RAM + swap** | 2 GB | 4 GB+ |
| **可用磁盘** | 6 GB | 10 GB+ |

如果你的主机低于最低要求，使用预构建二进制文件：

```bash
./install.sh --prefer-prebuilt
```

仅使用二进制安装，不回退到源码编译：

```bash
./install.sh --prebuilt-only
```

#### 可选

- **Docker** — 仅在使用 [Docker 沙箱运行时](#运行时支持当前)（`runtime.kind = "docker"`）时需要。通过你的包管理器或 [docker.com](https://docs.docker.com/engine/install/) 安装。

> **注意：** 默认的 `cargo build --release` 使用 `codegen-units=1` 以降低编译峰值压力。对于强大的机器，使用 `cargo build --profile release-fast` 加速构建。

</details>

<!-- markdownlint-enable MD001 MD024 -->

### 预构建二进制文件

发布资产可用于：

- Linux: `x86_64`、`aarch64`、`armv7`
- macOS: `x86_64`、`aarch64`
- Windows: `x86_64`

从以下位置下载最新资产：
<https://github.com/zeroclaw-labs/zeroclaw/releases/latest>

## 文档

当你完成引导流程后需要更深入的参考时使用这些文档。

- 从[文档索引](docs/README.md)开始了解导航和内容分布。
- 阅读[架构概述](docs/architecture.md)了解完整系统模型。
- 使用[配置参考](docs/reference/api/config-reference.md)查阅所有键和示例。
- 按照[运维手册](docs/ops/operations-runbook.md)运行 Gateway。
- 按照 [ZeroClaw Onboard](#快速开始简版) 进行引导设置。
- 使用[故障排除指南](docs/ops/troubleshooting.md)调试常见故障。
- 在暴露任何内容之前查看[安全指南](docs/security/README.md)。

### 参考文档

- 文档中心：[docs/README.md](docs/README.md)
- 统一文档目录：[docs/SUMMARY.md](docs/SUMMARY.md)
- 命令参考：[docs/reference/cli/commands-reference.md](docs/reference/cli/commands-reference.md)
- 配置参考：[docs/reference/api/config-reference.md](docs/reference/api/config-reference.md)
- 提供者参考：[docs/reference/api/providers-reference.md](docs/reference/api/providers-reference.md)
- 频道参考：[docs/reference/api/channels-reference.md](docs/reference/api/channels-reference.md)
- 运维手册：[docs/ops/operations-runbook.md](docs/ops/operations-runbook.md)
- 故障排除：[docs/ops/troubleshooting.md](docs/ops/troubleshooting.md)

### 协作文档

- 贡献指南：[CONTRIBUTING.md](CONTRIBUTING.md)
- PR 工作流策略：[docs/contributing/pr-workflow.md](docs/contributing/pr-workflow.md)
- CI 工作流指南：[docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- 审查员手册：[docs/contributing/reviewer-playbook.md](docs/contributing/reviewer-playbook.md)
- 安全披露策略：[SECURITY.md](SECURITY.md)
- 文档模板：[docs/contributing/doc-template.md](docs/contributing/doc-template.md)

### 部署 + 运维

- 网络部署指南：[docs/ops/network-deployment.md](docs/ops/network-deployment.md)
- 代理代理手册：[docs/ops/proxy-agent-playbook.md](docs/ops/proxy-agent-playbook.md)
- 硬件指南：[docs/hardware/README.md](docs/hardware/README.md)

## Smooth Crab 🦀

ZeroClaw 为 smooth crab 🦀 而构建，一个快速高效的 AI 助手。由 Argenis De La Rosa 和社区共同构建。

- [zeroclawlabs.ai](https://www.zeroclawlabs.ai)
- [@zeroclawlabs](https://x.com/zeroclawlabs)

## 支持 ZeroClaw

如果 ZeroClaw 对你的工作有帮助，你想支持持续开发，可以在这里捐款：

<a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=for-the-badge&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>

### 🙏 特别感谢

衷心感谢激励和推动这项开源工作的社区和机构：

- **哈佛大学** — 培养求知欲并推动可能性的边界。
- **MIT** — 倡导开放知识、开源以及技术应该人人可及的信念。
- **Sundai Club** — 社区、能量以及不懈追求构建有意义事物的动力。
- **世界及更远** 🌍✨ — 致每一位贡献者、梦想家和构建者，你们让开源成为一股向善的力量。这是献给你们的。

我们公开构建，因为最好的想法来自四面八方。如果你在阅读这些，你就是其中的一部分。欢迎。🦀❤️

## 贡献

ZeroClaw 新手？寻找标记为 [`good first issue`](https://github.com/zeroclaw-labs/zeroclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) 的问题 — 参阅我们的[贡献指南](CONTRIBUTING.md#first-time-contributors)了解如何开始。欢迎 AI/vibe-coded PR！🤖

参见 [CONTRIBUTING.md](CONTRIBUTING.md) 和 [CLA.md](docs/contributing/cla.md)。实现一个 trait，提交 PR：

- CI 工作流指南：[docs/contributing/ci-map.md](docs/contributing/ci-map.md)
- 新 `Provider` → `src/providers/`
- 新 `Channel` → `src/channels/`
- 新 `Observer` → `src/observability/`
- 新 `Tool` → `src/tools/`
- 新 `Memory` → `src/memory/`
- 新 `Tunnel` → `src/tunnel/`
- 新 `Peripheral` → `src/peripherals/`
- 新 `Skill` → `~/.zeroclaw/workspace/skills/<name>/`

<!-- BEGIN:RECENT_CONTRIBUTORS -->
<!-- END:RECENT_CONTRIBUTORS -->

## ⚠️ 官方仓库和冒充警告

**这是唯一的 ZeroClaw 官方仓库：**

> https://github.com/zeroclaw-labs/zeroclaw

任何其他声称是"ZeroClaw"或暗示与 ZeroClaw Labs 有关联的仓库、组织、域名或包都是**未经授权的，与本项目无关**。已知的未授权分支将在 [TRADEMARK.md](docs/maintainers/trademark.md) 中列出。

如果你遇到冒充或商标滥用，请[提交问题](https://github.com/zeroclaw-labs/zeroclaw/issues)。

---

## 许可证

ZeroClaw 采用双重许可，以实现最大开放性和贡献者保护：

| 许可证 | 使用场景 |
|--------|----------|
| [MIT](LICENSE-MIT) | 开源、研究、学术、个人使用 |
| [Apache 2.0](LICENSE-APACHE) | 专利保护、机构、商业部署 |

你可以选择任一许可证。**贡献者自动授予两种许可证的权利** — 参见 [CLA.md](docs/contributing/cla.md) 了解完整的贡献者协议。

### 商标

**ZeroClaw** 名称和标志是 ZeroClaw Labs 的商标。此许可证不授予使用它们暗示背书或关联的权限。参见 [TRADEMARK.md](docs/maintainers/trademark.md) 了解允许和禁止的使用。

### 贡献者保护

- 你**保留**你贡献的版权
- **专利授权**（Apache 2.0）保护你免受其他贡献者的专利索赔
- 你的贡献在提交历史和 [NOTICE](NOTICE) 中**永久归属**
- 贡献不转让商标权

---

**ZeroClaw** — 零开销。零妥协。随处部署。任意替换。🦀

## 贡献者

<a href="https://github.com/zeroclaw-labs/zeroclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=zeroclaw-labs/zeroclaw" alt="ZeroClaw contributors" />
</a>

此列表从 GitHub 贡献者图表生成，自动更新。

## Star 历史

<p align="center">
  <a href="https://www.star-history.com/#zeroclaw-labs/zeroclaw&type=date&legend=top-left">
    <picture>
     <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&theme=dark&legend=top-left" />
     <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
     <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=zeroclaw-labs/zeroclaw&type=date&legend=top-left" />
    </picture>
  </a>
</p>
