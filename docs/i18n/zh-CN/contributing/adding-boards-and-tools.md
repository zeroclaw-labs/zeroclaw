# 添加开发板和工具 — ZeroClaw 硬件指南

本指南解释如何向 ZeroClaw 添加新的硬件开发板和自定义工具。

## 快速开始：通过 CLI 添加开发板

```bash
# 添加开发板（更新 ~/.zeroclaw/config.toml）
zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
zeroclaw peripheral add arduino-uno /dev/cu.usbmodem12345
zeroclaw peripheral add rpi-gpio native   # 用于树莓派 GPIO（Linux）

# 重启守护进程应用更改
zeroclaw daemon --host 127.0.0.1 --port 42617
```

## 支持的开发板

| 开发板           | 传输方式 | 路径示例              |
|-----------------|-----------|---------------------------|
| nucleo-f401re   | 串口    | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno     | 串口    | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno-q   | 桥接    | （Uno Q IP 地址）                |
| rpi-gpio        | 原生    | native                    |
| esp32           | 串口    | /dev/ttyUSB0              |

## 手动配置

编辑 `~/.zeroclaw/config.toml`：

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets" # 可选：RAG 支持，用于将"打开红色 LED"映射到引脚 13

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "arduino-uno"
transport = "serial"
path = "/dev/cu.usbmodem12345"
baud = 115200
```

## 添加数据手册（RAG）

将 `.md` 或 `.txt` 文件放入 `docs/datasheets/`（或你的 `datasheet_dir`）。按开发板命名文件：`nucleo-f401re.md`、`arduino-uno.md`。

### 引脚别名（推荐）

添加 `## Pin Aliases` 部分，以便代理可以将"红色 LED"映射到引脚 13：

```markdown
# 我的开发板

## 引脚别名

| 别名       | 引脚 |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 5   |
```

或使用键值格式：

```markdown
## 引脚别名
red_led: 13
builtin_led: 13
```

### PDF 数据手册

使用 `rag-pdf` 特性时，ZeroClaw 可以索引 PDF 文件：

```bash
cargo build --features hardware,rag-pdf
```

将 PDF 放入数据手册目录。它们会被提取和分块用于 RAG（检索增强生成）。

## 添加新的开发板类型

1. **创建数据手册** — `docs/datasheets/my-board.md`，包含引脚别名和 GPIO（通用输入输出）信息。
2. **添加到配置** — `zeroclaw peripheral add my-board /dev/ttyUSB0`
3. **实现外设**（可选）—— 对于自定义协议，在 `src/peripherals/` 中实现 `Peripheral` 特征，并在 `create_peripheral_tools` 中注册。

完整设计请参见 [`docs/hardware/hardware-peripherals-design.md`](../hardware/hardware-peripherals-design.zh-CN.md)。

## 添加自定义工具

1. 在 `src/tools/` 中实现 `Tool` 特征。
2. 在 `create_peripheral_tools`（硬件工具）或代理工具注册表中注册。
3. 在 `src/agent/loop_.rs` 的代理 `tool_descs` 中添加工具描述。

## CLI 参考

| 命令 | 描述 |
|---------|-------------|
| `zeroclaw peripheral list` | 列出已配置的开发板 |
| `zeroclaw peripheral add <board> <path>` | 添加开发板（写入配置） |
| `zeroclaw peripheral flash` | 烧录 Arduino 固件 |
| `zeroclaw peripheral flash-nucleo` | 烧录 Nucleo 固件 |
| `zeroclaw hardware discover` | 列出 USB 设备 |
| `zeroclaw hardware info` | 通过 probe-rs 获取芯片信息 |

## 故障排除

- **找不到串口** — macOS 上使用 `/dev/cu.usbmodem*`；Linux 上使用 `/dev/ttyACM0` 或 `/dev/ttyUSB0`。
- **构建硬件支持** — `cargo build --features hardware`
- **Nucleo 支持 probe-rs** — `cargo build --features hardware,probe`
