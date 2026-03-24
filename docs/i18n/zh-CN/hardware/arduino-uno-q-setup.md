# Arduino Uno Q 上的 ZeroClaw — 分步指南

在 Arduino Uno Q 的 Linux 端运行 ZeroClaw。Telegram 通过 Wi-Fi 工作；GPIO 控制使用桥接（需要最小化的 App Lab 应用）。

---

## 已包含的内容（无需修改代码）

ZeroClaw 包含 Arduino Uno Q 所需的一切。**克隆仓库并按照本指南操作 —— 无需补丁或自定义代码。**

| 组件 | 位置 | 目的 |
|-----------|----------|---------|
| 桥接应用 | `firmware/uno-q-bridge/` | MCU 草图 + Python Socket 服务器（端口 9999）用于 GPIO |
| 桥接工具 | `src/peripherals/uno_q_bridge.rs` | 通过 TCP 与桥接通信的 `gpio_read` / `gpio_write` 工具 |
| 设置命令 | `src/peripherals/uno_q_setup.rs` | `zeroclaw peripheral setup-uno-q` 通过 scp + arduino-app-cli 部署桥接 |
| 配置 schema | `board = "arduino-uno-q"`, `transport = "bridge"` | 在 `config.toml` 中支持 |

使用 `--features hardware` 构建以包含 Uno Q 支持。

---

## 前置条件

- 已配置 Wi-Fi 的 Arduino Uno Q
- 安装在 Mac 上的 Arduino App Lab（用于初始设置和部署）
- LLM 的 API 密钥（OpenRouter 等）

---

## 阶段 1：Uno Q 初始设置（一次性）

### 1.1 通过 App Lab 配置 Uno Q

1. 下载 [Arduino App Lab](https://docs.arduino.cc/software/app-lab/)（Linux 上是 AppImage）。
2. 通过 USB 连接 Uno Q，开机。
3. 打开 App Lab，连接到开发板。
4. 按照设置向导操作：
   - 设置用户名和密码（用于 SSH）
   - 配置 Wi-Fi（SSID、密码）
   - 应用所有固件更新
5. 记录显示的 IP 地址（例如 `arduino@192.168.1.42`），或稍后在 App Lab 的终端中通过 `ip addr show` 查找。

### 1.2 验证 SSH 访问

```bash
ssh arduino@<UNO_Q_IP>
# 输入你设置的密码
```

---

## 阶段 2：在 Uno Q 上安装 ZeroClaw

### 选项 A：在设备上构建（更简单，约 20–40 分钟）

```bash
# SSH 进入 Uno Q
ssh arduino@<UNO_Q_IP>

# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# 安装构建依赖（Debian）
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev

# 克隆 zeroclaw（或 scp 你的项目）
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# 构建（在 Uno Q 上约 15–30 分钟）
cargo build --release --features hardware

# 安装
sudo cp target/release/zeroclaw /usr/local/bin/
```

### 选项 B：在 Mac 上交叉编译（更快）

```bash
# 在 Mac 上 — 添加 aarch64 目标
rustup target add aarch64-unknown-linux-gnu

# 安装交叉编译器（macOS；链接所需）
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu

# 构建
CC_aarch64_unknown_linux_gnu=aarch64-unknown-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu --features hardware

# 复制到 Uno Q
scp target/aarch64-unknown-linux-gnu/release/zeroclaw arduino@<UNO_Q_IP>:~/
ssh arduino@<UNO_Q_IP> "sudo mv ~/zeroclaw /usr/local/bin/"
```

如果交叉编译失败，使用选项 A 在设备上构建。

---

## 阶段 3：配置 ZeroClaw

### 3.1 运行引导配置（或手动创建配置）

```bash
ssh arduino@<UNO_Q_IP>

# 快速配置
zeroclaw onboard --api-key YOUR_OPENROUTER_KEY --provider openrouter

# 或手动创建配置
mkdir -p ~/.zeroclaw/workspace
nano ~/.zeroclaw/config.toml
```

### 3.2 最小化 config.toml

```toml
api_key = "YOUR_OPENROUTER_API_KEY"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"

[peripherals]
enabled = false
# 通过桥接使用 GPIO 需要完成阶段 4

[channels_config.telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
allowed_users = ["*"]

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false

[agent]
compact_context = true
```

---

## 阶段 4：运行 ZeroClaw 守护进程

```bash
ssh arduino@<UNO_Q_IP>

# 运行守护进程（Telegram 轮询通过 Wi-Fi 工作）
zeroclaw daemon --host 127.0.0.1 --port 42617
```

**此时：** Telegram 聊天正常工作。向你的机器人发送消息 —— ZeroClaw 会响应。还没有 GPIO 功能。

---

## 阶段 5：通过桥接实现 GPIO（ZeroClaw 自动处理）

ZeroClaw 包含桥接应用和设置命令。

### 5.1 部署桥接应用

**从你的 Mac**（在 zeroclaw 仓库中）：
```bash
zeroclaw peripheral setup-uno-q --host 192.168.0.48
```

**从 Uno Q**（已 SSH 连接）：
```bash
zeroclaw peripheral setup-uno-q
```

这会将桥接应用复制到 `~/ArduinoApps/uno-q-bridge` 并启动。

### 5.2 添加到 config.toml

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "arduino-uno-q"
transport = "bridge"
```

### 5.3 运行 ZeroClaw

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

现在当你向 Telegram 机器人发送 *"Turn on the LED"* 或 *"Set pin 13 high"* 时，ZeroClaw 会通过桥接使用 `gpio_write`。

---

## 命令摘要（从头到尾）

| 步骤 | 命令 |
|------|---------|
| 1 | 在 App Lab 中配置 Uno Q（Wi-Fi、SSH） |
| 2 | `ssh arduino@<IP>` |
| 3 | `curl -sSf https://sh.rustup.rs \| sh -s -- -y && source ~/.cargo/env` |
| 4 | `sudo apt-get install -y pkg-config libssl-dev` |
| 5 | `git clone https://github.com/zeroclaw-labs/zeroclaw.git && cd zeroclaw` |
| 6 | `cargo build --release --features hardware` |
| 7 | `zeroclaw onboard --api-key KEY --provider openrouter` |
| 8 | 编辑 `~/.zeroclaw/config.toml`（添加 Telegram bot_token） |
| 9 | `zeroclaw daemon --host 127.0.0.1 --port 42617` |
| 10 | 向 Telegram 机器人发送消息 —— 它会响应 |

---

## 故障排除

- **"command not found: zeroclaw"** — 使用完整路径：`/usr/local/bin/zeroclaw` 或确保 `~/.cargo/bin` 在 PATH 中。
- **Telegram 不响应** — 检查 bot_token、allowed_users，以及 Uno Q 有互联网连接（Wi-Fi）。
- **内存不足** — 保持特性最小化（Uno Q 使用 `--features hardware`）；考虑设置 `compact_context = true`。
- **GPIO 命令被忽略** — 确保桥接应用正在运行（`zeroclaw peripheral setup-uno-q` 会部署并启动它）。配置必须包含 `board = "arduino-uno-q"` 和 `transport = "bridge"`。
- **LLM 提供商（GLM/智谱）** — 使用 `default_provider = "glm"` 或 `"zhipu"`，并在环境或配置中设置 `GLM_API_KEY`。ZeroClaw 使用正确的 v4 端点。
