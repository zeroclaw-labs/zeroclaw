# 하드웨어 주변장치 설계 — ZeroClaw

ZeroClaw는 마이크로컨트롤러(MCU)와 싱글 board 컴퓨터(SBC)가 **자연어 명령을 동적으로 해석**하고, 하드웨어 전용 코드를 생성하며, 주변장치 상호작용을 실시간으로 실행할 수 있도록 합니다.

## 1. 비전

**목표:** ZeroClaw는 하드웨어 인식 AI 에이전트로서 다음을 수행합니다:
- 채널(WhatsApp, Telegram)을 통해 자연어 트리거를 수신합니다 (예: "X 팔 움직여", "LED 켜줘")
- 정확한 하드웨어 문서(데이터시트, 레지스터 맵)를 가져옵니다
- LLM(Gemini, 로컬 오픈소스 모델)을 사용하여 Rust 코드/로직을 합성합니다
- 로직을 실행하여 주변장치(GPIO, I2C, SPI)를 조작합니다
- 향후 재사용을 위해 최적화된 코드를 저장합니다

**개념 모델:** ZeroClaw = 하드웨어를 이해하는 두뇌. 주변장치 = 두뇌가 제어하는 팔과 다리.

## 2. 두 가지 운영 모드

### 모드 1: Edge-Native (독립 실행형)

**대상:** Wi-Fi 지원 board (ESP32, Raspberry Pi).

ZeroClaw가 **기기에서 직접** 실행됩니다. board가 gRPC/nanoRPC 서버를 시작하고 주변장치와 로컬로 통신합니다.

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

**워크플로우:**
1. 사용자가 WhatsApp으로 전송: *"핀 13의 LED 켜줘"*
2. ZeroClaw가 board 전용 문서를 가져옵니다 (예: ESP32 GPIO 매핑)
3. LLM이 Rust 코드를 합성합니다
4. 코드가 샌드박스(Wasm 또는 동적 링킹)에서 실행됩니다
5. GPIO가 토글되고 결과가 사용자에게 반환됩니다
6. 최적화된 코드가 향후 "LED 켜줘" 요청을 위해 저장됩니다

**모든 작업이 기기에서 수행됩니다.** 호스트가 필요하지 않습니다.

### 모드 2: Host-Mediated (개발 / 디버깅)

**대상:** USB / J-Link / Aardvark를 통해 호스트(macOS, Linux)에 연결된 하드웨어.

ZeroClaw가 **호스트**에서 실행되며 대상 기기와 하드웨어 인식 링크를 유지합니다. 개발, 검사, flash에 사용됩니다.

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

**워크플로우:**
1. 사용자가 Telegram으로 전송: *"이 USB 장치에서 읽을 수 있는 메모리 주소는 무엇인가요?"*
2. ZeroClaw가 연결된 하드웨어를 식별합니다 (VID/PID, 아키텍처)
3. 메모리 매핑을 수행하고 사용 가능한 주소 공간을 제안합니다
4. 결과를 사용자에게 반환합니다

**또는:**
1. 사용자: *"이 firmware를 Nucleo에 flash 해줘"*
2. ZeroClaw가 OpenOCD 또는 probe-rs를 통해 쓰기/flash를 수행합니다
3. 성공을 확인합니다

**또는:**
1. ZeroClaw가 자동 검색: *"STM32 Nucleo가 /dev/ttyACM0에 있으며, ARM Cortex-M4입니다"*
2. 제안: *"GPIO, ADC, flash를 읽고 쓸 수 있습니다. 무엇을 하시겠습니까?"*

---

### 모드 비교

| 항목              | Edge-Native                    | Host-Mediated                    |
|------------------|--------------------------------|----------------------------------|
| ZeroClaw 실행 위치 | 기기 (ESP32, RPi)           | 호스트 (Mac, Linux)                |
| 하드웨어 연결    | 로컬 (GPIO, I2C, SPI)        | USB, J-Link, Aardvark            |
| LLM              | 기기 내 또는 클라우드 (Gemini)   | 호스트 (클라우드 또는 로컬)            |
| 사용 사례         | 프로덕션, 독립 실행형         | 개발, 디버깅, 검사       |
| 채널         | WhatsApp 등 (Wi-Fi 경유)      | Telegram, CLI 등              |

## 3. 레거시 / 간단한 모드 (Edge에서 LLM 실행 이전)

Wi-Fi가 없는 board이거나 완전한 Edge-Native 준비가 되기 전:

### 모드 A: 호스트 + 원격 주변장치 (STM32 시리얼 통신)

호스트에서 ZeroClaw를 실행하고, 주변장치에서는 최소한의 firmware를 실행합니다. 시리얼을 통한 간단한 JSON 통신입니다.

### 모드 B: RPi를 호스트로 사용 (네이티브 GPIO)

Pi에서 ZeroClaw를 실행하고, rppal 또는 sysfs를 통해 GPIO를 제어합니다. 별도의 firmware가 필요하지 않습니다.

## 4. 기술 요구사항

| 요구사항 | 설명 |
|-------------|-------------|
| **언어** | 순수 Rust. 임베디드 타겟(STM32, ESP32)에는 해당 시 `no_std` 사용. |
| **통신** | 저지연 명령 처리를 위한 경량 gRPC 또는 nanoRPC 스택. |
| **동적 실행** | LLM이 생성한 로직을 즉시 안전하게 실행: 격리를 위한 Wasm 런타임 또는 지원되는 경우 동적 링킹. |
| **문서 검색** | 데이터시트 스니펫, 레지스터 맵, 핀아웃을 LLM 컨텍스트에 제공하는 RAG(Retrieval-Augmented Generation) 파이프라인. |
| **하드웨어 검색** | USB 장치용 VID/PID 기반 식별; 아키텍처 감지 (ARM Cortex-M, RISC-V 등). |

### RAG 파이프라인 (데이터시트 검색)

- **색인:** 데이터시트, 레퍼런스 매뉴얼, 레지스터 맵 (PDF를 청크로 분할, 임베딩).
- **검색:** 사용자 쿼리 시("LED 켜줘") 관련 스니펫을 가져옵니다 (예: 대상 board의 GPIO 섹션).
- **주입:** LLM 시스템 프롬프트 또는 컨텍스트에 추가합니다.
- **결과:** LLM이 정확한 board 전용 코드를 생성합니다.

### 동적 실행 옵션

| 옵션 | 장점 | 단점 |
|-------|------|------|
| **Wasm** | 샌드박스, 이식성, FFI 불필요 | 오버헤드; Wasm에서 제한된 HW 접근 |
| **동적 링킹** | 네이티브 속도, 전체 HW 접근 | 플랫폼 의존적; 보안 우려 |
| **인터프리터 DSL** | 안전, 감사 가능 | 느림; 제한된 표현력 |
| **사전 컴파일된 템플릿** | 빠름, 안전 | 유연성 부족; 템플릿 라이브러리 필요 |

**권장사항:** 사전 컴파일된 템플릿 + 파라미터화로 시작하고, 안정화되면 사용자 정의 로직을 위해 Wasm으로 발전시킵니다.

## 5. CLI 및 설정

### CLI 플래그

```bash
# Edge-Native: 기기에서 실행 (ESP32, RPi)
zeroclaw agent --mode edge

# Host-Mediated: USB/J-Link 대상에 연결
zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
zeroclaw agent --probe jlink

# 하드웨어 검사
zeroclaw hardware discover
zeroclaw hardware introspect /dev/ttyACM0
```

### 설정 (config.toml)

```toml
[peripherals]
enabled = true
mode = "host"  # "edge" | "host"
datasheet_dir = "docs/datasheets"  # RAG: LLM 컨텍스트를 위한 board 전용 문서

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
# Edge-Native: ZeroClaw가 ESP32에서 실행
```

## 6. 아키텍처: 주변장치를 확장 포인트로

### 새 Trait: `Peripheral`

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

### 흐름

1. **시작:** ZeroClaw가 설정을 로드하고 `peripherals.boards`를 확인합니다.
2. **연결:** 각 board에 대해 `Peripheral` 구현체를 생성하고 `connect()`를 호출합니다.
3. **도구:** 연결된 모든 주변장치에서 도구를 수집하고 기본 도구와 병합합니다.
4. **에이전트 루프:** 에이전트가 `gpio_write`, `sensor_read` 등을 호출할 수 있으며, 이는 주변장치에 위임됩니다.
5. **종료:** 각 주변장치에 대해 `disconnect()`를 호출합니다.

### Board 지원

| Board              | 전송 방식 | Firmware / 드라이버      | 도구                    |
|--------------------|-----------|------------------------|--------------------------|
| nucleo-f401re      | serial    | Zephyr / Embassy       | gpio_read, gpio_write, adc_read |
| rpi-gpio           | native    | rppal 또는 sysfs         | gpio_read, gpio_write    |
| esp32              | serial/ws | ESP-IDF / Embassy      | gpio, wifi, mqtt         |

## 7. 통신 프로토콜

### gRPC / nanoRPC (Edge-Native, Host-Mediated)

ZeroClaw와 주변장치 간의 저지연, 타입 지정 RPC:

- **nanoRPC** 또는 **tonic** (gRPC): Protobuf으로 정의된 서비스.
- 메서드: `GpioWrite`, `GpioRead`, `I2cTransfer`, `SpiTransfer`, `MemoryRead`, `FlashWrite` 등.
- 스트리밍, 양방향 호출, `.proto` 파일에서의 코드 생성을 지원합니다.

### Serial 폴백 (Host-Mediated, 레거시)

gRPC를 지원하지 않는 board를 위한 간단한 JSON 시리얼 통신:

**요청 (호스트 → 주변장치):**
```json
{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}
```

**응답 (주변장치 → 호스트):**
```json
{"id":"1","ok":true,"result":"done"}
```

## 8. Firmware (별도 저장소 또는 Crate)

- **zeroclaw-firmware** 또는 **zeroclaw-peripheral** — 별도의 crate/workspace.
- 타겟: `thumbv7em-none-eabihf` (STM32), `armv7-unknown-linux-gnueabihf` (RPi) 등.
- STM32에는 `embassy` 또는 Zephyr를 사용합니다.
- 위의 프로토콜을 구현합니다.
- 사용자가 board에 flash하면, ZeroClaw가 연결하여 기능을 검색합니다.

## 9. 구현 단계

### 1단계: 스켈레톤 ✅ (완료)

- [x] `Peripheral` trait, 설정 스키마, CLI (`zeroclaw peripheral list/add`) 추가
- [x] 에이전트에 `--peripheral` 플래그 추가
- [x] AGENTS.md에 문서화

### 2단계: Host-Mediated — 하드웨어 검색 ✅ (완료)

- [x] `zeroclaw hardware discover`: USB 장치 열거 (VID/PID)
- [x] Board 레지스트리: VID/PID → 아키텍처, 이름 매핑 (예: Nucleo-F401RE)
- [x] `zeroclaw hardware introspect <path>`: 메모리 맵, 주변장치 목록

### 3단계: Host-Mediated — Serial / J-Link

- [x] USB CDC를 통한 STM32용 `SerialPeripheral`
- [ ] flash/디버깅을 위한 probe-rs 또는 OpenOCD 통합
- [x] 도구: `gpio_read`, `gpio_write` (memory_read, flash_write는 향후)

### 4단계: RAG 파이프라인 ✅ (완료)

- [x] 데이터시트 색인 (markdown/text를 청크로 분할)
- [x] 하드웨어 관련 쿼리 시 LLM 컨텍스트에 검색 및 주입
- [x] Board 전용 프롬프트 보강

**사용법:** config.toml의 `[peripherals]`에 `datasheet_dir = "docs/datasheets"`를 추가합니다. board 이름으로 된 `.md` 또는 `.txt` 파일을 배치합니다 (예: `nucleo-f401re.md`, `rpi-gpio.md`). `_generic/`에 있거나 `generic.md`라는 이름의 파일은 모든 board에 적용됩니다. 키워드 매칭으로 청크를 검색하여 사용자 메시지 컨텍스트에 주입합니다.

### 5단계: Edge-Native — RPi ✅ (완료)

- [x] Raspberry Pi에서 ZeroClaw (rppal을 통한 네이티브 GPIO)
- [ ] 로컬 주변장치 접근을 위한 gRPC/nanoRPC 서버
- [ ] 코드 저장 (합성된 스니펫 저장)

### 6단계: Edge-Native — ESP32

- [x] Host-mediated ESP32 (시리얼 전송) — STM32와 동일한 JSON 프로토콜
- [x] `esp32` firmware crate (`firmware/esp32`) — UART를 통한 GPIO
- [x] 하드웨어 레지스트리에 ESP32 등록 (CH340 VID/PID)
- [ ] ESP32 *위에서* ZeroClaw 실행 (WiFi + LLM, edge-native) — 향후
- [ ] LLM 생성 로직을 위한 Wasm 또는 템플릿 기반 실행

**사용법:** ESP32에 `firmware/esp32`를 flash하고, 설정에 `board = "esp32"`, `transport = "serial"`, `path = "/dev/ttyUSB0"`를 추가합니다.

### 7단계: 동적 실행 (LLM 생성 코드)

- [ ] 템플릿 라이브러리: 파라미터화된 GPIO/I2C/SPI 스니펫
- [ ] 선택 사항: 사용자 정의 로직을 위한 Wasm 런타임 (샌드박스)
- [ ] 최적화된 코드 경로 저장 및 재사용

## 10. 보안 고려사항

- **시리얼 경로:** `path`가 허용 목록에 있는지 확인합니다 (예: `/dev/ttyACM*`, `/dev/ttyUSB*`); 임의 경로는 절대 불가.
- **GPIO:** 노출되는 핀을 제한하고, 전원/리셋 핀은 피합니다.
- **주변장치에 비밀 정보 없음:** firmware에 API 키를 저장하지 않으며, 호스트가 인증을 처리합니다.

## 11. 비목표 (현재)

- 베어메탈 STM32에서 전체 ZeroClaw 실행 (Wi-Fi 없음, 제한된 RAM) — 대신 Host-Mediated 사용
- 실시간 보장 — 주변장치는 최선 노력 방식
- LLM에서의 임의 네이티브 코드 실행 — Wasm 또는 템플릿 선호

## 12. 관련 문서

- [adding-boards-and-tools.md](../contributing/adding-boards-and-tools.md) — board 및 데이터시트 추가 방법
- [network-deployment.md](../ops/network-deployment.md) — RPi 및 네트워크 배포

## 13. 참고 자료

- [Zephyr RTOS Rust support](https://docs.zephyrproject.org/latest/develop/languages/rust/index.html)
- [Embassy](https://embassy.dev/) — 비동기 임베디드 프레임워크
- [rppal](https://github.com/golemparts/rppal) — Rust로 작성된 Raspberry Pi GPIO
- [STM32 Nucleo-F401RE](https://www.st.com/en/evaluation-tools/nucleo-f401re.html)
- [tonic](https://github.com/hyperium/tonic) — Rust용 gRPC
- [probe-rs](https://probe.rs/) — ARM 디버그 프로브, flash, 메모리 접근
- [nusb](https://github.com/nic-hartley/nusb) — USB 장치 열거 (VID/PID)

## 14. 원본 프롬프트 요약

> *"ESP, Raspberry Pi 또는 Wi-Fi를 갖춘 board는 LLM(Gemini 또는 오픈소스)에 연결할 수 있습니다. ZeroClaw가 기기에서 실행되어 자체 gRPC를 생성하고 시작하며 주변장치와 통신합니다. 사용자가 WhatsApp으로 'X 팔 움직여' 또는 'LED 켜줘'라고 요청합니다. ZeroClaw가 정확한 문서를 가져오고, 코드를 작성하고, 실행하고, 최적으로 저장하고, 실행하여 LED를 켭니다 — 모두 개발 board에서 이루어집니다.*
>
> *Mac에 USB/J-Link/Aardvark로 연결된 STM Nucleo의 경우: Mac에서 ZeroClaw가 하드웨어에 접근하여 기기에 원하는 것을 설치하거나 쓰고 결과를 반환합니다. 예: 'ZeroClaw, 이 USB 장치에서 사용 가능한/읽을 수 있는 주소는 무엇인가요?' 어디에 무엇이 연결되어 있는지 파악하고 제안할 수 있습니다."*
