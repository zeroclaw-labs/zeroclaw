# 硬件外设设计 — ZeroClaw

ZeroClaw 让微控制器（MCU，Microcontroller Unit）和单板计算机（SBC，Single Board Computer）能够**动态解释自然语言命令**，生成硬件特定代码，并实时执行外设交互。

## 1. 愿景

**目标：** ZeroClaw 作为具备硬件感知能力的 AI 代理，能够：
- 通过渠道（WhatsApp、Telegram）接收自然语言触发（例如"移动 X 机械臂"、"打开 LED"）
- 获取准确的硬件文档（数据手册、寄存器映射）
- 使用 LLM（大语言模型，如 Gemini、本地开源模型）合成 Rust 代码/逻辑
- 执行逻辑操作外设（GPIO、I2C、SPI）
- 持久化优化后的代码供未来复用

**思维模型：** ZeroClaw = 理解硬件的大脑。外设 = 它控制的手臂和腿。

## 2. 两种运行模式

### 模式 1：边缘原生（独立运行）

**目标：** 支持 Wi-Fi 的开发板（ESP32、树莓派）。

ZeroClaw **直接运行在设备上**。开发板启动 gRPC/nanoRPC 服务器，与本地外设通信。

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  ZeroClaw on ESP32 / Raspberry Pi (Edge-Native)                             │
│                                                                             │
│  ┌─────────────┐    ┌──────────────┐    ┌─────────────────────────────────┐ │
│  │ Channels    │───►│ Agent Loop   │───►│ RAG: datasheets, register maps  │ │
│  │ WhatsApp    │    │ (LLM calls)  │    │ → LLM context                    │ │
│  │ Telegram    │    └──────┬───────┘    └─────────────────────────────────┘ │
│  └─────────────┘           │                                                 │
│                            ▼                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │ Code synthesis → Wasm / dynamic exec → GPIO / I2C / SPI → persist       ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│                                                                             │
│  gRPC/nanoRPC server ◄──► Peripherals (GPIO, I2C, SPI, sensors, actuators)  │
└─────────────────────────────────────────────────────────────────────────────┘
```

**工作流：**
1. 用户发送 WhatsApp 消息：*"打开引脚 13 上的 LED"*
2. ZeroClaw 获取开发板特定文档（例如 ESP32 GPIO 映射）
3. LLM 合成 Rust 代码
4. 代码在沙箱中运行（Wasm 或动态链接）
5. GPIO 被切换；结果返回给用户
6. 优化后的代码被持久化，供未来"打开 LED"请求使用

**所有操作都在设备上完成。** 不需要主机。

### 模式 2：主机介导（开发/调试）

**目标：** 通过 USB / J-Link / Aardvark 连接到主机（macOS、Linux）的硬件。

ZeroClaw 运行在**主机**上，并维护到目标的硬件感知链接。用于开发、内省和烧录。

```
┌─────────────────────┐                    ┌──────────────────────────────────┐
│  ZeroClaw on Mac    │   USB / J-Link /   │  STM32 Nucleo-F401RE              │
│                     │   Aardvark         │  (or other MCU)                    │
│  - Channels         │ ◄────────────────► │  - Memory map                     │
│  - LLM              │                    │  - Peripherals (GPIO, ADC, I2C)    │
│  - Hardware probe   │   VID/PID          │  - Flash / RAM                     │
│  - Flash / debug    │   discovery        │                                    │
└─────────────────────┘                    └──────────────────────────────────┘
```

**工作流：**
1. 用户发送 Telegram 消息：*"这个 USB 设备上的可读内存地址是什么？"*
2. ZeroClaw 识别连接的硬件（VID/PID、架构）
3. 执行内存映射；建议可用的地址空间
4. 将结果返回给用户

**或：**
1. 用户：*"将这个固件烧录到 Nucleo"*
2. ZeroClaw 通过 OpenOCD 或 probe-rs 写入/烧录
3. 确认成功

**或：**
1. ZeroClaw 自动发现：*"STM32 Nucleo 位于 /dev/ttyACM0，ARM Cortex-M4"*
2. 建议：*"我可以读取/写入 GPIO、ADC、闪存。你想做什么？"*

---

### 模式对比

| 方面           | 边缘原生                    | 主机介导                    |
|------------------|--------------------------------|----------------------------------|
| ZeroClaw 运行位置 | 设备（ESP32、树莓派）           | 主机（Mac、Linux）                |
| 硬件链接    | 本地（GPIO、I2C、SPI）        | USB、J-Link、Aardvark            |
| LLM              | 设备端或云端（Gemini）   | 主机（云端或本地）            |
| 使用场景         | 生产环境、独立运行         | 开发、调试、内省       |
| 渠道         | WhatsApp 等（通过 Wi-Fi）      | Telegram、CLI 等              |

## 3. 传统/简单模式（边缘 LLM 之前）

对于没有 Wi-Fi 的开发板，或在边缘原生模式完全就绪之前：

### 模式 A：主机 + 远程外设（通过串口的 STM32）

主机运行 ZeroClaw；外设运行最小化固件。通过串口传输简单 JSON。

### 模式 B：树莓派作为主机（原生 GPIO）

ZeroClaw 运行在树莓派上；通过 rppal 或 sysfs 访问 GPIO。不需要单独的固件。

## 4. 技术要求

| 要求 | 描述 |
|-------------|-------------|
| **语言** | 纯 Rust。嵌入式目标（STM32、ESP32）适用时使用 `no_std`。 |
| **通信** | 轻量级 gRPC 或 nanoRPC 栈，用于低延迟命令处理。 |
| **动态执行** | 安全地即时运行 LLM 生成的逻辑：用于隔离的 Wasm 运行时，或支持时使用动态链接。 |
| **文档检索** | RAG（检索增强生成）流水线，将数据手册片段、寄存器映射和引脚定义输入到 LLM 上下文。 |
| **硬件发现** | USB 设备基于 VID/PID 的识别；架构检测（ARM Cortex-M、RISC-V 等）。 |

### RAG 流水线（数据手册检索）

- **索引：** 数据手册、参考手册、寄存器映射（PDF → 分块、嵌入向量）。
- **检索：** 用户查询（"打开 LED"）时，获取相关片段（例如目标开发板的 GPIO 部分）。
- **注入：** 添加到 LLM 系统提示或上下文。
- **结果：** LLM 生成准确的、开发板特定的代码。

### 动态执行选项

| 选项 | 优点 | 缺点 |
|-------|------|------|
| **Wasm** | 沙箱化、可移植、无 FFI | 开销大；Wasm 对硬件访问有限 |
| **动态链接** | 原生速度、完全硬件访问 | 平台特定；安全隐患 |
| **解释型 DSL** | 安全、可审计 | 速度慢；表达能力有限 |
| **预编译模板** | 快速、安全 | 灵活性较低；需要模板库 |

**建议：** 从预编译模板 + 参数化开始；稳定后演进到 Wasm 支持用户自定义逻辑。

## 5. CLI 和配置

### CLI 标志

```bash
# 边缘原生：在设备上运行（ESP32、树莓派）
zeroclaw agent --mode edge

# 主机介导：连接到 USB/J-Link 目标
zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
zeroclaw agent --probe jlink

# 硬件内省
zeroclaw hardware discover
zeroclaw hardware introspect /dev/ttyACM0
```

### 配置（config.toml）

```toml
[peripherals]
enabled = true
mode = "host"  # "edge" | "host"
datasheet_dir = "docs/datasheets"  # RAG: 供 LLM 上下文使用的开发板特定文档

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"

[[peripherals.boards]]
board = "esp32"
transport = "wifi"
# 边缘原生：ZeroClaw 运行在 ESP32 上
```

## 6. 架构：外设作为扩展点

### 新特征：`Peripheral`

```rust
/// A hardware peripheral that exposes capabilities as tools.
#[async_trait]
pub trait Peripheral: Send + Sync {
    fn name(&self) -> &str;
    fn board_type(&self) -> &str;  // e.g. "nucleo-f401re", "rpi-gpio"
    async fn connect(&mut self) -> anyhow::Result<()>;
    async fn disconnect(&mut self) -> anyhow::Result<()>;
    async fn health_check(&self) -> bool;
    /// Tools this peripheral provides (gpio_read, gpio_write, sensor_read, etc.)
    fn tools(&self) -> Vec<Box<dyn Tool>>;
}
```

### 流程

1. **启动：** ZeroClaw 加载配置，读取 `peripherals.boards`。
2. **连接：** 为每个开发板创建 `Peripheral` 实现，调用 `connect()`。
3. **工具：** 收集所有连接外设的工具；与默认工具合并。
4. **代理循环：** 代理可以调用 `gpio_write`、`sensor_read` 等 —— 这些调用委托给外设。
5. **关闭：** 对每个外设调用 `disconnect()`。

### 开发板支持

| 开发板              | 传输方式 | 固件 / 驱动      | 工具                    |
|--------------------|-----------|------------------------|--------------------------|
| nucleo-f401re      | 串口    | Zephyr / Embassy       | gpio_read, gpio_write, adc_read |
| rpi-gpio           | 原生    | rppal or sysfs         | gpio_read, gpio_write    |
| esp32              | 串口/websocket | ESP-IDF / Embassy      | gpio, wifi, mqtt         |

## 7. 通信协议

### gRPC / nanoRPC（边缘原生、主机介导）

用于 ZeroClaw 和外设之间的低延迟、类型化 RPC：

- **nanoRPC** 或 **tonic**（gRPC）：Protobuf 定义的服务。
- 方法：`GpioWrite`、`GpioRead`、`I2cTransfer`、`SpiTransfer`、`MemoryRead`、`FlashWrite` 等。
- 支持流、双向调用和从 `.proto` 文件生成代码。

### 串口回退（主机介导、传统）

对于不支持 gRPC 的开发板，通过串口传输简单 JSON：

**请求（主机 → 外设）：**
```json
{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}
```

**响应（外设 → 主机）：**
```json
{"id":"1","ok":true,"result":"done"}
```

## 8. 固件（独立仓库或 crate）

- **zeroclaw-firmware** 或 **zeroclaw-peripheral** —— 独立的 crate/工作区。
- 目标：`thumbv7em-none-eabihf`（STM32）、`armv7-unknown-linux-gnueabihf`（树莓派）等。
- STM32 使用 `embassy` 或 Zephyr。
- 实现上述协议。
- 用户将其烧录到开发板；ZeroClaw 连接并发现能力。

## 9. 实现阶段

### 阶段 1：骨架 ✅（已完成）

- [x] 添加 `Peripheral` 特征、配置 schema、CLI（`zeroclaw peripheral list/add`）
- [x] 为代理添加 `--peripheral` 标志
- [x] 在 AGENTS.md 中记录

### 阶段 2：主机介导 — 硬件发现 ✅（已完成）

- [x] `zeroclaw hardware discover`：枚举 USB 设备（VID/PID）
- [x] 开发板注册表：映射 VID/PID → 架构、名称（例如 Nucleo-F401RE）
- [x] `zeroclaw hardware introspect <path>`：内存映射、外设列表

### 阶段 3：主机介导 — 串口 / J-Link

- [x] 支持通过 USB CDC 连接 STM32 的 `SerialPeripheral`
- [ ] 集成 probe-rs 或 OpenOCD 用于烧录/调试
- [x] 工具：`gpio_read`、`gpio_write`（未来支持 memory_read、flash_write）

### 阶段 4：RAG 流水线 ✅（已完成）

- [x] 数据手册索引（markdown/text → 分块）
- [x] 硬件相关查询时检索并注入到 LLM 上下文
- [x] 开发板特定提示增强

**用法：** 在 config.toml 的 `[peripherals]` 部分添加 `datasheet_dir = "docs/datasheets"`。按开发板命名放置 `.md` 或 `.txt` 文件（例如 `nucleo-f401re.md`、`rpi-gpio.md`）。`_generic/` 目录下或名为 `generic.md` 的文件适用于所有开发板。通过关键词匹配检索分块并注入到用户消息上下文。

### 阶段 5：边缘原生 — 树莓派 ✅（已完成）

- [x] 树莓派上的 ZeroClaw（通过 rppal 实现原生 GPIO）
- [ ] 用于本地外设访问的 gRPC/nanoRPC 服务器
- [ ] 代码持久化（存储合成的片段）

### 阶段 6：边缘原生 — ESP32

- [x] 主机介导的 ESP32（串口传输）—— 与 STM32 相同的 JSON 协议
- [x] `esp32` 固件 crate（`firmware/esp32`）—— 通过 UART 实现 GPIO
- [x] 硬件注册表中的 ESP32（CH340 VID/PID）
- [ ] ESP32 上运行 ZeroClaw（Wi-Fi + LLM，边缘原生）—— 未来
- [ ] 基于 Wasm 或模板的 LLM 生成逻辑执行

**用法：** 将 `firmware/esp32` 烧录到 ESP32，在配置中添加 `board = "esp32"`、`transport = "serial"`、`path = "/dev/ttyUSB0"`。

### 阶段 7：动态执行（LLM 生成代码）

- [ ] 模板库：参数化的 GPIO/I2C/SPI 片段
- [ ] 可选：用于用户自定义逻辑的 Wasm 运行时（沙箱化）
- [ ] 持久化和复用优化的代码路径

## 10. 安全考虑

- **串口路径：** 验证 `path` 在白名单中（例如 `/dev/ttyACM*`、`/dev/ttyUSB*`）；永远不允许任意路径。
- **GPIO：** 限制暴露的引脚；避免电源/复位引脚。
- **外设上无密钥：** 固件不应存储 API 密钥；主机处理认证。

## 11. 非目标（目前）

- 在裸 STM32 上运行完整 ZeroClaw（无 Wi-Fi、RAM 有限）—— 改用主机介导模式
- 实时保证 —— 外设是尽力而为的
- LLM 生成的任意原生代码执行 —— 优先使用 Wasm 或模板

## 12. 相关文档

- [adding-boards-and-tools.md](../contributing/adding-boards-and-tools.zh-CN.md) — 如何添加开发板和数据手册
- [network-deployment.md](../ops/network-deployment.zh-CN.md) — 树莓派和网络部署

## 13. 参考

- [Zephyr RTOS Rust support](https://docs.zephyrproject.org/latest/develop/languages/rust/index.html)
- [Embassy](https://embassy.dev/) — 异步嵌入式框架
- [rppal](https://github.com/golemparts/rppal) — Rust 实现的树莓派 GPIO
- [STM32 Nucleo-F401RE](https://www.st.com/en/evaluation-tools/nucleo-f401re.html)
- [tonic](https://github.com/hyperium/tonic) — Rust 实现的 gRPC
- [probe-rs](https://probe.rs/) — ARM 调试探针、烧录、内存访问
- [nusb](https://github.com/nic-hartley/nusb) — USB 设备枚举（VID/PID）

## 14. 原始提示词摘要

> *"像 ESP、树莓派或带 Wi-Fi 的开发板可以连接到 LLM（Gemini 或开源模型）。ZeroClaw 运行在设备上，创建自己的 gRPC 服务，启动服务并与外设通信。用户通过 WhatsApp 询问：'移动 X 机械臂'或'打开 LED'。ZeroClaw 获取准确的文档，编写代码，执行它，优化存储，运行并打开 LED —— 所有操作都在开发板上完成。*
>
> *对于通过 USB/J-Link/Aardvark 连接到我 Mac 的 STM Nucleo：我 Mac 上的 ZeroClaw 访问硬件，在设备上安装或写入想要的内容，并返回结果。示例：'嘿 ZeroClaw，这个 USB 设备上的可用/可读地址是什么？'它能找出连接的内容和位置并给出建议。"*
