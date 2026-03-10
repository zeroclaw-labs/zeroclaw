# Hardware Peripherals Design — ZeroClaw

ZeroClaw enables microcontrollers (MCUs) and Single Board Computers (SBCs) to **dynamically interpret natural language commands**, generate hardware-specific code, and execute peripheral interactions in real-time.

## 1. Vision

**Goal:** ZeroClaw acts as a hardware-aware AI agent that:
- Receives natural language triggers (e.g. "Move X arm", "Turn on LED") via channels (WhatsApp, Telegram)
- Fetches accurate hardware documentation (datasheets, register maps)
- Synthesizes Rust code/logic using an LLM (Gemini, local open-source models)
- Executes the logic to manipulate peripherals (GPIO, I2C, SPI)
- Persists optimized code for future reuse

**Mental model:** ZeroClaw = brain that understands hardware. Peripherals = arms and legs it controls.

## 2. Two Modes of Operation

### Mode 1: Edge-Native (Standalone)

**Target:** Wi-Fi-enabled boards (ESP32, Raspberry Pi).

ZeroClaw runs **directly on the device**. The board spins up a gRPC/nanoRPC server and communicates with peripherals locally.

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

**Workflow:**
1. User sends WhatsApp: *"Turn on LED on pin 13"*
2. ZeroClaw fetches board-specific docs (e.g. ESP32 GPIO mapping)
3. LLM synthesizes Rust code
4. Code runs in a sandbox (Wasm or dynamic linking)
5. GPIO is toggled; result returned to user
6. Optimized code is persisted for future "Turn on LED" requests

**All happens on-device.** No host required.

### Mode 2: Host-Mediated (Development / Debugging)

**Target:** Hardware connected via USB / J-Link / Aardvark to a host (macOS, Linux).

ZeroClaw runs on the **host** and maintains a hardware-aware link to the target. Used for development, introspection, and flashing.

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

**Workflow:**
1. User sends Telegram: *"What are the readable memory addresses on this USB device?"*
2. ZeroClaw identifies connected hardware (VID/PID, architecture)
3. Performs memory mapping; suggests available address spaces
4. Returns result to user

**Or:**
1. User: *"Flash this firmware to the Nucleo"*
2. ZeroClaw writes/flashes via OpenOCD or probe-rs
3. Confirms success

**Or:**
1. ZeroClaw auto-discovers: *"STM32 Nucleo on /dev/ttyACM0, ARM Cortex-M4"*
2. Suggests: *"I can read/write GPIO, ADC, flash. What would you like to do?"*

---

### Mode Comparison

| Aspect           | Edge-Native                    | Host-Mediated                    |
|------------------|--------------------------------|----------------------------------|
| ZeroClaw runs on | Device (ESP32, RPi)           | Host (Mac, Linux)                |
| Hardware link    | Local (GPIO, I2C, SPI)        | USB, J-Link, Aardvark            |
| LLM              | On-device or cloud (Gemini)   | Host (cloud or local)            |
| Use case         | Production, standalone         | Dev, debug, introspection       |
| Channels         | WhatsApp, etc. (via WiFi)      | Telegram, CLI, etc.              |

## 3. Legacy / Simpler Modes (Pre-LLM-on-Edge)

For boards without WiFi or before full Edge-Native is ready:

### Mode A: Host + Remote Peripheral (STM32 via serial)

Host runs ZeroClaw; peripheral runs minimal firmware. Simple JSON over serial.

### Mode B: RPi as Host (Native GPIO)

ZeroClaw on Pi; GPIO via rppal or sysfs. No separate firmware.

## 4. Technical Requirements

| Requirement | Description |
|-------------|-------------|
| **Language** | Pure Rust. `no_std` where applicable for embedded targets (STM32, ESP32). |
| **Communication** | Lightweight gRPC or nanoRPC stack for low-latency command processing. |
| **Dynamic execution** | Safely run LLM-generated logic on-the-fly: Wasm runtime for isolation, or dynamic linking where supported. |
| **Documentation retrieval** | RAG (Retrieval-Augmented Generation) pipeline to feed datasheet snippets, register maps, and pinouts into LLM context. |
| **Hardware discovery** | VID/PID-based identification for USB devices; architecture detection (ARM Cortex-M, RISC-V, etc.). |

### RAG Pipeline (Datasheet Retrieval)

- **Index:** Datasheets, reference manuals, register maps (PDF → chunks, embeddings).
- **Retrieve:** On user query ("turn on LED"), fetch relevant snippets (e.g. GPIO section for target board).
- **Inject:** Add to LLM system prompt or context.
- **Result:** LLM generates accurate, board-specific code.

### Dynamic Execution Options

| Option | Pros | Cons |
|-------|------|------|
| **Wasm** | Sandboxed, portable, no FFI | Overhead; limited HW access from Wasm |
| **Dynamic linking** | Native speed, full HW access | Platform-specific; security concerns |
| **Interpreted DSL** | Safe, auditable | Slower; limited expressiveness |
| **Pre-compiled templates** | Fast, secure | Less flexible; requires template library |

**Recommendation:** Start with pre-compiled templates + parameterization; evolve to Wasm for user-defined logic once stable.

## 5. CLI and Config

### CLI Flags

```bash
# Edge-Native: run on device (ESP32, RPi)
zeroclaw agent --mode edge

# Host-Mediated: connect to USB/J-Link target
zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
zeroclaw agent --probe jlink

# Hardware introspection
zeroclaw hardware discover
zeroclaw hardware introspect /dev/ttyACM0
```

### Config (config.toml)

```toml
[peripherals]
enabled = true
mode = "host"  # "edge" | "host"
datasheet_dir = "docs/datasheets"  # RAG: board-specific docs for LLM context

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
# Edge-Native: ZeroClaw runs on ESP32
```

## 6. Architecture: Peripheral as Extension Point

### New Trait: `Peripheral`

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

### Flow

1. **Startup:** ZeroClaw loads config, sees `peripherals.boards`.
2. **Connect:** For each board, create a `Peripheral` impl, call `connect()`.
3. **Tools:** Collect tools from all connected peripherals; merge with default tools.
4. **Agent loop:** Agent can call `gpio_write`, `sensor_read`, etc. — these delegate to the peripheral.
5. **Shutdown:** Call `disconnect()` on each peripheral.

### Board Support

| Board              | Transport | Firmware / Driver      | Tools                    |
|--------------------|-----------|------------------------|--------------------------|
| nucleo-f401re      | serial    | Zephyr / Embassy       | gpio_read, gpio_write, adc_read |
| rpi-gpio           | native    | rppal or sysfs         | gpio_read, gpio_write    |
| esp32              | serial/ws | ESP-IDF / Embassy      | gpio, wifi, mqtt         |

## 7. Communication Protocols

### gRPC / nanoRPC (Edge-Native, Host-Mediated)

For low-latency, typed RPC between ZeroClaw and peripherals:

- **nanoRPC** or **tonic** (gRPC): Protobuf-defined services.
- Methods: `GpioWrite`, `GpioRead`, `I2cTransfer`, `SpiTransfer`, `MemoryRead`, `FlashWrite`, etc.
- Enables streaming, bidirectional calls, and code generation from `.proto` files.

### Serial Fallback (Host-Mediated, legacy)

Simple JSON over serial for boards without gRPC support:

**Request (host → peripheral):**
```json
{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}
```

**Response (peripheral → host):**
```json
{"id":"1","ok":true,"result":"done"}
```

## 8. Firmware (Separate Repo or Crate)

- **zeroclaw-firmware** or **zeroclaw-peripheral** — a separate crate/workspace.
- Targets: `thumbv7em-none-eabihf` (STM32), `armv7-unknown-linux-gnueabihf` (RPi), etc.
- Uses `embassy` or Zephyr for STM32.
- Implements the protocol above.
- User flashes this to the board; ZeroClaw connects and discovers capabilities.

## 9. Implementation Phases

### Phase 1: Skeleton ✅ (Done)

- [x] Add `Peripheral` trait, config schema, CLI (`zeroclaw peripheral list/add`)
- [x] Add `--peripheral` flag to agent
- [x] Document in AGENTS.md

### Phase 2: Host-Mediated — Hardware Discovery ✅ (Done)

- [x] `zeroclaw hardware discover`: enumerate USB devices (VID/PID)
- [x] Board registry: map VID/PID → architecture, name (e.g. Nucleo-F401RE)
- [x] `zeroclaw hardware introspect <path>`: memory map, peripheral list

### Phase 3: Host-Mediated — Serial / J-Link

- [x] `SerialPeripheral` for STM32 over USB CDC
- [ ] probe-rs or OpenOCD integration for flash/debug
- [x] Tools: `gpio_read`, `gpio_write` (memory_read, flash_write in future)

### Phase 4: RAG Pipeline ✅ (Done)

- [x] Datasheet index (markdown/text → chunks)
- [x] Retrieve-and-inject into LLM context on hardware-related queries
- [x] Board-specific prompt augmentation

**Usage:** Add `datasheet_dir = "docs/datasheets"` to `[peripherals]` in config.toml. Place `.md` or `.txt` files named by board (e.g. `nucleo-f401re.md`, `rpi-gpio.md`). Files in `_generic/` or named `generic.md` apply to all boards. Chunks are retrieved by keyword match and injected into the user message context.

### Phase 5: Edge-Native — RPi ✅ (Done)

- [x] ZeroClaw on Raspberry Pi (native GPIO via rppal)
- [ ] gRPC/nanoRPC server for local peripheral access
- [ ] Code persistence (store synthesized snippets)

### Phase 6: Edge-Native — ESP32

- [x] Host-mediated ESP32 (serial transport) — same JSON protocol as STM32
- [x] `zeroclaw-esp32` firmware crate (`firmware/zeroclaw-esp32`) — GPIO over UART
- [x] ESP32 in hardware registry (CH340 VID/PID)
- [ ] ZeroClaw *on* ESP32 (WiFi + LLM, edge-native) — future
- [ ] Wasm or template-based execution for LLM-generated logic

**Usage:** Flash `firmware/zeroclaw-esp32` to ESP32, add `board = "esp32"`, `transport = "serial"`, `path = "/dev/ttyUSB0"` to config.

### Phase 7: Dynamic Execution (LLM-Generated Code)

- [ ] Template library: parameterized GPIO/I2C/SPI snippets
- [ ] Optional: Wasm runtime for user-defined logic (sandboxed)
- [ ] Persist and reuse optimized code paths

## 10. Security Considerations

- **Serial path:** Validate `path` is in allowlist (e.g. `/dev/ttyACM*`, `/dev/ttyUSB*`); never arbitrary paths.
- **GPIO:** Restrict which pins are exposed; avoid power/reset pins.
- **No secrets on peripheral:** Firmware should not store API keys; host handles auth.

## 11. Non-Goals (For Now)

- Running full ZeroClaw *on* bare STM32 (no WiFi, limited RAM) — use Host-Mediated instead
- Real-time guarantees — peripherals are best-effort
- Arbitrary native code execution from LLM — prefer Wasm or templates

## 12. Related Documents

- [adding-boards-and-tools.md](./adding-boards-and-tools.md) — How to add boards and datasheets
- [network-deployment.md](./network-deployment.md) — RPi and network deployment

## 13. References

- [Zephyr RTOS Rust support](https://docs.zephyrproject.org/latest/develop/languages/rust/index.html)
- [Embassy](https://embassy.dev/) — async embedded framework
- [rppal](https://github.com/golemparts/rppal) — Raspberry Pi GPIO in Rust
- [STM32 Nucleo-F401RE](https://www.st.com/en/evaluation-tools/nucleo-f401re.html)
- [tonic](https://github.com/hyperium/tonic) — gRPC for Rust
- [probe-rs](https://probe.rs/) — ARM debug probe, flash, memory access
- [nusb](https://github.com/nic-hartley/nusb) — USB device enumeration (VID/PID)

## 14. Raw Prompt Summary

> *"Boards like ESP, Raspberry Pi, or boards with WiFi can connect to an LLM (Gemini or open-source). ZeroClaw runs on the device, creates its own gRPC, spins it up, and communicates with peripherals. User asks via WhatsApp: 'move X arm' or 'turn on LED'. ZeroClaw gets accurate documentation, writes code, executes it, stores it optimally, runs it, and turns on the LED — all on the development board.*
>
> *For STM Nucleo connected via USB/J-Link/Aardvark to my Mac: ZeroClaw from my Mac accesses the hardware, installs or writes what it wants on the device, and returns the result. Example: 'Hey ZeroClaw, what are the available/readable addresses on this USB device?' It can figure out what's connected where and suggest."*
