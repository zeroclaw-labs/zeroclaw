# ZeroClaw 部署安装指南

## 简介

ZeroClaw 是一个高性能、低资源占用的 Rust 编写的 AI Agent 运行时。它具有以下特点：

- **超低资源占用**：运行内存 < 5MB，二进制体积仅约 8.8MB
- **快速启动**：冷启动时间 < 10ms
- **跨平台**：支持 ARM、x86、RISC-V 架构
- **安全默认**：内置配对鉴权、沙箱隔离、文件系统作用域限制

## 环境要求

### 硬件要求

| 资源 | 最低配置 | 推荐配置 |
|------|----------|----------|
| 内存 + Swap | 2 GB | 4 GB+ |
| 磁盘空间 | 6 GB | 10 GB+ |

### 系统支持

- **Linux**：Debian、Ubuntu、Fedora、RHEL、Alpine
- **macOS**：x86_64、ARM64 (Apple Silicon)
- **Windows**：x86_64 (需要 Visual Studio Build Tools)

---

## 安装方式一：Homebrew（推荐 macOS/Linux）

最简单的方式是通过 Homebrew 安装：

```bash
# 安装 ZeroClaw
brew install zeroclaw

# 验证安装
zeroclaw --version
```

---

## 安装方式二：一键部署脚本（推荐）

### 方式 A：克隆仓库后运行本地脚本（推荐）

```bash
# 1. 克隆仓库
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# 2. 运行一键部署脚本
./bootstrap.sh
```

这个脚本会：
1. 自动检测系统环境
2. 安装必要的构建依赖（需要 Rust 工具链）
3. 编译并安装 ZeroClaw

### 方式 B：远程一键安装

```bash
# 直接运行远程脚本（安全敏感环境建议使用方式 A）
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash
```

---

## 安装方式三：手动编译安装

### 第一步：安装系统依赖

#### Linux (Debian/Ubuntu)

```bash
sudo apt update
sudo apt install build-essential pkg-config curl
```

#### Linux (Fedora/RHEL)

```bash
sudo dnf group install development-tools
sudo dnf install pkg-config curl
```

#### macOS

```bash
# 安装 Xcode Command Line Tools
xcode-select --install
```

#### Windows

1. 安装 Visual Studio Build Tools：
   ```powershell
   winget install Microsoft.VisualStudio.2022.BuildTools
   ```

2. 安装时选择 **"Desktop development with C++"** 工作负载

### 第二步：安装 Rust 工具链

```bash
# 安装 Rust（所有平台）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

安装完成后，在新的终端中验证：

```bash
rustc --version
cargo --version
```

### 第三步：编译并安装 ZeroClaw

```bash
# 1. 克隆仓库
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# 2. 编译（Release 模式）
cargo build --release --locked

# 3. 安装到系统
cargo install --path . --force --locked

# 4. 确保 PATH 包含 cargo bin 目录
export PATH="$HOME/.cargo/bin:$PATH"

# 5. 验证安装
zeroclaw --version
```

---

## 安装方式四：预编译二进制

如果不想编译，可以直接下载预编译的二进制文件：

### 下载地址

访问 [GitHub Releases](https://github.com/zeroclaw-labs/zeroclaw/releases/latest) 下载对应平台的二进制文件。

支持的平台：
- **Linux**：x86_64、aarch64、armv7
- **macOS**：x86_64、aarch64
- **Windows**：x86_64

### 安装示例（Linux ARM64）

```bash
# 下载
curl -fsSLO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-unknown-linux-gnu.tar.gz

# 解压
tar xzf zeroclaw-aarch64-unknown-linux-gnu.tar.gz

# 安装到 ~/.cargo/bin
install -m 0755 zeroclaw "$HOME/.cargo/bin/zeroclaw"

# 验证
zeroclaw --version
```

---

## 首次配置

### 方式一：快速配置（非交互式）

```bash
# 替换为你的 API Key 和提供商
zeroclaw onboard --api-key "sk-your-api-key" --provider openrouter
```

### 方式二：交互式配置

```bash
zeroclaw onboard --interactive
```

按提示完成配置，包括：
- 选择 AI 模型提供商
- 输入 API Key
- 配置消息通道（Telegram、Discord 等）
- 设置允许的用户列表

### 方式三：仅配置通道

如果已有配置文件，只想修复通道配置：

```bash
zeroclaw onboard --channels-only
```

---

## 常用命令

### 基本操作

```bash
# 查看状态
zeroclaw status

# 系统诊断
zeroclaw doctor

# 启动交互式对话
zeroclaw agent

# 发送单条消息
zeroclaw agent -m "你好 ZeroClaw！"

# 启动网关服务（默认 127.0.0.1:42617）
zeroclaw gateway

# 启动守护进程（长期运行模式）
zeroclaw daemon
```

### 通道管理

```bash
# 查看支持的通道
zeroclaw channel list

# 启动通道
zeroclaw channel start

# 检查通道健康状态
zeroclaw channel doctor

# 绑定 Telegram 用户到白名单
zeroclaw channel bind-telegram 123456789
```

### 服务管理

```bash
# 安装为系统服务（Linux systemd）
zeroclaw service install

# 启动服务
zeroclaw service start

# 查看服务状态
zeroclaw service status

# 重启服务
zeroclaw service restart
```

### 其他实用命令

```bash
# 生成 shell 补全脚本
source <(zeroclaw completions bash)

# 查看支持的提供商
zeroclaw providers

# 刷新模型列表
zeroclaw models refresh

# 检查认证状态
zeroclaw auth status
```

---

## 配置说明

ZeroClaw 的配置文件位于 `~/.zeroclaw/config.toml`。主要配置项：

```toml
# API 配置
api_key = "sk-your-api-key"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.7

# 记忆系统配置
[memory]
backend = "sqlite"  # sqlite | lucid | postgres | markdown | none
auto_save = true
embedding_provider = "none"  # none | openai | custom

# 网关配置
[gateway]
host = "127.0.0.1"
port = 42617
require_pairing = true  # 首次连接需要配对码
allow_public_bind = false  # 禁止公网绑定

# 自主性配置
[autonomy]
level = "supervised"  # readonly | supervised | full
workspace_only = true  # 限制在工作目录内
allowed_commands = ["git", "ls", "cat", "grep"]

# 运行时配置
[runtime]
kind = "native"  # native | docker
```

---

## 通道配置示例

### Telegram

```toml
[channels_config.telegram]
enabled = true
bot_token = "your-bot-token"
allowed_users = ["your-telegram-username"]  # 不带 @ 符号
```

### Discord

```toml
[channels_config.discord]
enabled = true
bot_token = "your-bot-token"
allowed_users = ["your-discord-user-id"]
```

### WhatsApp

```toml
[channels_config.whatsapp]
enabled = true
# 方式一：WhatsApp Web（扫码配对）
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
allowed_numbers = ["+1234567890"]

# 方式二：WhatsApp Business API
access_token = "EAABx..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
```

---

## 验证安装

完成安装后，运行以下命令验证：

```bash
# 1. 查看版本
zeroclaw --version

# 2. 查看状态
zeroclaw status

# 3. 运行诊断
zeroclaw doctor

# 4. 快速测试
zeroclaw agent -m "Hello!"
```

---

## 常见问题

### Q: 编译时内存不足怎么办？

A: 使用预编译二进制：
```bash
./bootstrap.sh --prefer-prebuilt
```

### Q: 不想编译，只想用二进制怎么办？

A: 
```bash
./bootstrap.sh --prebuilt-only
```

### Q: 如何在 Docker 中运行？

A:
```bash
./bootstrap.sh --docker
```

### Q: 首次使用需要配置什么？

A: 至少需要：
1. 一个 API Key（来自 OpenRouter、Anthropic、OpenAI 等）
2. 选择一个模型提供商

### Q: 如何更新 ZeroClaw？

A: 如果通过 Homebrew 安装：
```bash
brew upgrade zeroclaw
```

如果手动安装，重新编译：
```bash
cargo install --path . --force --locked
```

---

## 下一步

- 查看详细命令参考：[commands-reference.md](docs/commands-reference.md)
- 查看配置参考：[config-reference.md](docs/config-reference.md)
- 查看提供商列表：[providers-reference.md](docs/providers-reference.md)
- 运维手册：[operations-runbook.md](docs/operations-runbook.md)
- 故障排查：[troubleshooting.md](docs/troubleshooting.md)

---

## 相关链接

- 官方仓库：https://github.com/zeroclaw-labs/zeroclaw
- 官网：https://zeroclawlabs.ai
- 文档中心：https://github.com/zeroclaw-labs/zeroclaw/tree/main/docs
