# 网络部署 — 树莓派和本地网络上的 ZeroClaw

本文档介绍如何在树莓派或本地网络上的其他主机上部署 ZeroClaw，支持 Telegram 和可选的 webhook 渠道。

---

## 1. 概述

| 模式 | 需要入站端口？ | 使用场景 |
|------|----------------------|----------|
| **Telegram 轮询** | 否 | ZeroClaw 轮询 Telegram API；可在任何地方工作 |
| **Matrix 同步（包括 E2EE）** | 否 | ZeroClaw 通过 Matrix 客户端 API 同步；不需要入站 webhook |
| **Discord/Slack** | 否 | 相同 — 仅出站连接 |
| **Nostr** | 否 | 通过 WebSocket 连接到中继；仅出站连接 |
| **网关 webhook** | 是 | POST /webhook、/whatsapp、/linq、/nextcloud-talk 需要公共 URL |
| **网关配对** | 是 | 如果你通过网关配对客户端 |
| **Alpine/OpenRC 服务** | 否 | Alpine Linux 上的系统级后台服务 |

**关键点：** Telegram、Discord、Slack 和 Nostr 使用**出站连接** — ZeroClaw 连接到外部服务器/中继。不需要端口转发或公共 IP。

---

## 2. 树莓派上的 ZeroClaw

### 2.1 前置条件

- 安装了 Raspberry Pi OS 的树莓派（3/4/5）
- USB 外围设备（Arduino、Nucleo）如果使用串口传输
- 可选：用于原生 GPIO 的 `rppal`（`peripheral-rpi` 特性）

### 2.2 安装

```bash
# 为 RPi 构建（或从主机交叉编译）
cargo build --release --features hardware

# 或通过你偏好的方法安装
```

### 2.3 配置

编辑 `~/.zeroclaw/config.toml`：

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = \"rpi-gpio\"
transport = \"native\"

# 或通过 USB 连接的 Arduino
[[peripherals.boards]]
board = \"arduino-uno\"
transport = \"serial\"
path = \"/dev/ttyACM0\"
baud = 115200

[channels_config.telegram]
bot_token = \"YOUR_BOT_TOKEN\"
allowed_users = []

[gateway]
host = \"127.0.0.1\"
port = 42617
allow_public_bind = false
```

### 2.4 运行守护进程（仅本地）

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

- 网关绑定到 `127.0.0.1` — 其他机器无法访问
- Telegram 渠道工作正常：ZeroClaw 轮询 Telegram API（出站）
- 不需要防火墙或端口转发

---

## 3. 绑定到 0.0.0.0（本地网络）

要允许 LAN 上的其他设备访问网关（例如用于配对或 webhook）：

### 3.1 选项 A：显式选择加入

```toml
[gateway]
host = \"0.0.0.0\"
port = 42617
allow_public_bind = true
```

```bash
zeroclaw daemon --host 0.0.0.0 --port 42617
```

**安全提示：** `allow_public_bind = true` 会将网关暴露给你的本地网络。仅在受信任的 LAN 上使用。

### 3.2 选项 B：隧道（推荐用于 Webhook）

如果你需要**公共 URL**（例如 WhatsApp webhook、外部客户端）：

1. 在本地主机上运行网关：
   ```bash
   zeroclaw daemon --host 127.0.0.1 --port 42617
   ```

2. 启动隧道：
   ```toml
   [tunnel]
   provider = \"tailscale\"   # 或 \"ngrok\"、\"cloudflare\"
   ```
   或使用 `zeroclaw tunnel`（参见隧道文档）。

3. 除非 `allow_public_bind = true` 或隧道处于活动状态，否则 ZeroClaw 会拒绝绑定到 `0.0.0.0`。

---

## 4. Telegram 轮询（无入站端口）

Telegram 默认使用**长轮询**：

- ZeroClaw 调用 `https://api.telegram.org/bot{token}/getUpdates`
- 不需要入站端口或公共 IP
- 可在 NAT 后、RPi 上、家庭实验室中工作

**配置：**

```toml
[channels_config.telegram]
bot_token = \"YOUR_BOT_TOKEN\"
allowed_users = []            # 默认拒绝，显式绑定身份
```

运行 `zeroclaw daemon` — Telegram 渠道会自动启动。

要在运行时批准一个 Telegram 账户：

```bash
zeroclaw channel bind-telegram <IDENTITY>
```

`<IDENTITY>` 可以是数字 Telegram 用户 ID 或用户名（不带 `@`）。

### 4.1 单轮询器规则（重要）

Telegram Bot API `getUpdates` 每个机器人令牌仅支持一个活动轮询器。

- 为同一个令牌仅保留一个运行时实例（推荐：`zeroclaw daemon` 服务）。
- 不要同时运行 `cargo run -- channel start` 或其他机器人进程。

如果遇到此错误：

`Conflict: terminated by other getUpdates request`

说明你有轮询冲突。停止额外实例并仅重启一个守护进程。

---

## 5. Webhook 渠道（WhatsApp、Nextcloud Talk、自定义）

基于 Webhook 的渠道需要**公共 URL**，以便 Meta（WhatsApp）或你的客户端可以 POST 事件。

### 5.1 Tailscale Funnel

```toml
[tunnel]
provider = \"tailscale\"
```

Tailscale Funnel 通过 `*.ts.net` URL 暴露你的网关。无需端口转发。

### 5.2 ngrok

```toml
[tunnel]
provider = \"ngrok\"
```

或手动运行 ngrok：
```bash
ngrok http 42617
# 将 HTTPS URL 用于你的 webhook
```

### 5.3 Cloudflare Tunnel

配置 Cloudflare Tunnel 转发到 `127.0.0.1:42617`，然后将你的 webhook URL 设置为隧道的公共主机名。

---

## 6. 检查清单：RPi 部署

- [ ] 使用 `--features hardware` 构建（如果使用原生 GPIO 则添加 `peripheral-rpi`）
- [ ] 配置 `[peripherals]` 和 `[channels_config.telegram]`
- [ ] 运行 `zeroclaw daemon --host 127.0.0.1 --port 42617`（Telegram 不需要 0.0.0.0 即可工作）
- [ ] 用于 LAN 访问：`--host 0.0.0.0` + 配置中设置 `allow_public_bind = true`
- [ ] 用于 webhook：使用 Tailscale、ngrok 或 Cloudflare 隧道

---

## 7. OpenRC（Alpine Linux 服务）

ZeroClaw 支持 Alpine Linux 和其他使用 OpenRC 初始化系统的发行版的 OpenRC。OpenRC 服务**系统级**运行，需要 root/sudo。

### 7.1 前置条件

- Alpine Linux（或其他基于 OpenRC 的发行版）
- Root 或 sudo 访问权限
- 专用的 `zeroclaw` 系统用户（安装期间创建）

### 7.2 安装服务

```bash
# 安装服务（Alpine 上会自动检测 OpenRC）
sudo zeroclaw service install
```

这会创建：
- 初始化脚本：`/etc/init.d/zeroclaw`
- 配置目录：`/etc/zeroclaw/`
- 日志目录：`/var/log/zeroclaw/`

### 7.3 配置

通常不需要手动复制配置。

`sudo zeroclaw service install` 会自动准备 `/etc/zeroclaw`，如果有可用的用户设置，会迁移现有运行时状态，并为 `zeroclaw` 服务用户设置所有权/权限。

如果没有可迁移的现有运行时状态，请在启动服务前创建 `/etc/zeroclaw/config.toml`。

### 7.4 启用和启动

```bash
# 添加到默认运行级别
sudo rc-update add zeroclaw default

# 启动服务
sudo rc-service zeroclaw start

# 检查状态
sudo rc-service zeroclaw status
```

### 7.5 管理服务

| 命令 | 描述 |
|---------|-------------|
| `sudo rc-service zeroclaw start` | 启动守护进程 |
| `sudo rc-service zeroclaw stop` | 停止守护进程 |
| `sudo rc-service zeroclaw status` | 检查服务状态 |
| `sudo rc-service zeroclaw restart` | 重启守护进程 |
| `sudo zeroclaw service status` | ZeroClaw 状态包装器（使用 `/etc/zeroclaw` 配置） |

### 7.6 日志

OpenRC 将日志路由到：

| 日志 | 路径 |
|-----|------|
| 访问/stdout | `/var/log/zeroclaw/access.log` |
| 错误/stderr | `/var/log/zeroclaw/error.log` |

查看日志：

```bash
sudo tail -f /var/log/zeroclaw/error.log
```

### 7.7 卸载

```bash
# 停止并从运行级别移除
sudo rc-service zeroclaw stop
sudo rc-update del zeroclaw default

# 移除初始化脚本
sudo zeroclaw service uninstall
```

### 7.8 注意事项

- OpenRC **仅系统级**（无用户级服务）
- 所有服务操作都需要 `sudo` 或 root
- 服务以 `zeroclaw:zeroclaw` 用户运行（最小权限原则）
- 配置必须位于 `/etc/zeroclaw/config.toml`（初始化脚本中的显式路径）
- 如果 `zeroclaw` 用户不存在，安装会失败并提供创建说明

### 7.9 检查清单：Alpine/OpenRC 部署

- [ ] 安装：`sudo zeroclaw service install`
- [ ] 启用：`sudo rc-update add zeroclaw default`
- [ ] 启动：`sudo rc-service zeroclaw start`
- [ ] 验证：`sudo rc-service zeroclaw status`
- [ ] 检查日志：`/var/log/zeroclaw/error.log`

---

## 8. 参考文档

- [channels-reference.zh-CN.md](../reference/api/channels-reference.zh-CN.md) — 渠道配置概述
- [matrix-e2ee-guide.zh-CN.md](../security/matrix-e2ee-guide.zh-CN.md) — Matrix 安装和加密房间故障排除
- [hardware-peripherals-design.zh-CN.md](../hardware/hardware-peripherals-design.zh-CN.md) — 外围设备设计
- [adding-boards-and-tools.zh-CN.md](../contributing/adding-boards-and-tools.zh-CN.md) — 硬件安装和添加板卡
