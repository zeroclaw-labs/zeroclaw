# Nucleo-F401RE 上的 JhedaiClaw — 分步指南

在 Mac 或 Linux 主机上运行 JhedaiClaw。通过 USB 连接 Nucleo-F401RE。通过 Telegram 或 CLI 控制 GPIO（LED、引脚）。

---

## 通过 Telegram 获取开发板信息（无需固件）

JhedaiClaw 可以通过 USB 从 Nucleo 读取芯片信息，**无需烧录任何固件**。向你的 Telegram 机器人发送消息：

- _"我有什么开发板信息？"_
- _"开发板信息"_
- _"连接了什么硬件？"_
- _"芯片信息"_

代理使用 `hardware_board_info` 工具返回芯片名称、架构和内存映射。启用 `probe` 特性时，它会通过 USB/SWD 读取实时数据；否则返回静态数据手册信息。

**配置：** 首先将 Nucleo 添加到 `config.toml`（以便代理知道查询哪个开发板）：

```toml
[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200
```

**CLI 替代方案：**

```bash
cargo build --features hardware,probe
jhedaiclaw hardware info
jhedaiclaw hardware discover
```

---

## 已包含的内容（无需修改代码）

JhedaiClaw 包含 Nucleo-F401RE 所需的一切：

| 组件     | 位置                                 | 目的                                                   |
| -------- | ------------------------------------ | ------------------------------------------------------ |
| 固件     | `firmware/nucleo/`                   | Embassy Rust — USART2（115200）、gpio_read、gpio_write |
| 串门外设 | `src/peripherals/serial.rs`          | 基于串口的 JSON 协议（与 Arduino/ESP32 相同）          |
| 烧录命令 | `jhedaiclaw peripheral flash-nucleo` | 构建固件，通过 probe-rs 烧录                           |

协议：换行符分隔的 JSON。请求：`{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`。响应：`{"id":"1","ok":true,"result":"done"}`。

---

## 前置条件

- Nucleo-F401RE 开发板
- USB 线（USB-A 转 Mini-USB；Nucleo 内置 ST-Link）
- 烧录所需：`cargo install probe-rs-tools --locked`（或使用[安装脚本](https://probe.rs/docs/getting-started/installation/)）

---

## 阶段 1：烧录固件

### 1.1 连接 Nucleo

1. 通过 USB 将 Nucleo 连接到 Mac/Linux。
2. 开发板会显示为 USB 设备（ST-Link）。现代系统不需要单独的驱动。

### 1.2 通过 JhedaiClaw 烧录

在 jhedaiclaw 仓库根目录执行：

```bash
jhedaiclaw peripheral flash-nucleo
```

这会构建 `firmware/nucleo` 并运行 `probe-rs run --chip STM32F401RETx`。固件烧录后立即运行。

### 1.3 手动烧录（替代方案）

```bash
cd firmware/nucleo
cargo build --release --target thumbv7em-none-eabihf
probe-rs run --chip STM32F401RETx target/thumbv7em-none-eabihf/release/nucleo
```

---

## 阶段 2：查找串口

- **macOS：** `/dev/cu.usbmodem*` 或 `/dev/tty.usbmodem*`（例如 `/dev/cu.usbmodem101`）
- **Linux：** `/dev/ttyACM0`（或插入后查看 `dmesg`）

USART2（PA2/PA3）桥接到 ST-Link 的虚拟 COM 端口，因此主机看到一个串口设备。

---

## 阶段 3：配置 JhedaiClaw

添加到 `~/.jhedaiclaw/config.toml`：

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/cu.usbmodem101"   # 调整为你的端口
baud = 115200
```

---

## 阶段 4：运行和测试

```bash
jhedaiclaw daemon --host 127.0.0.1 --port 42617
```

或直接使用代理：

```bash
jhedaiclaw agent --message "Turn on the LED on pin 13"
```

引脚 13 = PA5 = Nucleo-F401RE 上的用户 LED（LD2）。

---

## 命令摘要

| 步骤 | 命令                                                       |
| ---- | ---------------------------------------------------------- |
| 1    | 通过 USB 连接 Nucleo                                       |
| 2    | `cargo install probe-rs-tools --locked`                    |
| 3    | `jhedaiclaw peripheral flash-nucleo`                       |
| 4    | 将 Nucleo 添加到 config.toml（path = 你的串口）            |
| 5    | `jhedaiclaw daemon` 或 `jhedaiclaw agent -m "Turn on LED"` |

---

## 故障排除

- **flash-nucleo 无法识别** — 从仓库构建：`cargo run --features hardware -- peripheral flash-nucleo`。该子命令仅在仓库构建中包含，crates.io 安装版本不包含。
- **找不到 probe-rs** — `cargo install probe-rs-tools --locked`（`probe-rs` crate 是库；CLI 在 `probe-rs-tools` 中）
- **未检测到探针** — 确保 Nucleo 已连接。尝试其他 USB 线/端口。
- **找不到串口** — 在 Linux 上，将用户添加到 `dialout` 组：`sudo usermod -a -G dialout $USER`，然后注销/登录。
- **GPIO 命令被忽略** — 检查配置中的 `path` 与你的串口匹配。运行 `jhedaiclaw peripheral list` 验证。
