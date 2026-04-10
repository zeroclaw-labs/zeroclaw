# Nucleo-F401RE에서 ZeroClaw 사용하기 — 단계별 가이드

Mac 또는 Linux 호스트에서 ZeroClaw를 실행합니다. Nucleo-F401RE를 USB로 연결합니다. Telegram 또는 CLI를 통해 GPIO(LED, 핀)를 제어합니다.

---

## Telegram으로 Board 정보 확인 (Firmware 불필요)

ZeroClaw는 **firmware를 flash하지 않고도** USB를 통해 Nucleo의 칩 정보를 읽을 수 있습니다. Telegram 봇에 다음과 같이 메시지를 보냅니다:

- *"Board 정보 알려줘"*
- *"Board info"*
- *"어떤 하드웨어가 연결되어 있어?"*
- *"칩 정보"*

에이전트는 `hardware_board_info` 도구를 사용하여 칩 이름, 아키텍처, 메모리 맵을 반환합니다. `probe` 기능이 있으면 USB/SWD를 통해 실시간 데이터를 읽고, 그렇지 않으면 정적 데이터시트 정보를 반환합니다.

**설정:** 먼저 `config.toml`에 Nucleo를 추가합니다 (에이전트가 어떤 board를 조회할지 알 수 있도록):

```toml
[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200
```

**CLI 대안:**

```bash
cargo build --features hardware,probe
zeroclaw hardware info
zeroclaw hardware discover
```

---

## 포함된 구성 요소 (코드 변경 불필요)

ZeroClaw에는 Nucleo-F401RE에 필요한 모든 것이 포함되어 있습니다:

| 구성 요소 | 위치 | 용도 |
|-----------|----------|---------|
| Firmware | `firmware/nucleo/` | Embassy Rust — USART2 (115200), gpio_read, gpio_write |
| Serial 주변장치 | `src/peripherals/serial.rs` | JSON-over-serial 프로토콜 (Arduino/ESP32와 동일) |
| Flash 명령어 | `zeroclaw peripheral flash-nucleo` | firmware를 빌드하고 probe-rs를 통해 flash |

프로토콜: 개행 문자로 구분된 JSON. 요청: `{"id":"1","cmd":"gpio_write","args":{"pin":13,"value":1}}`. 응답: `{"id":"1","ok":true,"result":"done"}`.

---

## 사전 요구사항

- Nucleo-F401RE board
- USB 케이블 (USB-A to Mini-USB; Nucleo에는 ST-Link가 내장되어 있습니다)
- flash용: `cargo install probe-rs-tools --locked` (또는 [설치 스크립트](https://probe.rs/docs/getting-started/installation/) 사용)

---

## 1단계: Firmware Flash

### 1.1 Nucleo 연결

1. Nucleo를 USB로 Mac/Linux에 연결합니다.
2. board가 USB 장치(ST-Link)로 나타납니다. 최신 시스템에서는 별도의 드라이버가 필요하지 않습니다.

### 1.2 ZeroClaw를 통한 Flash

zeroclaw 저장소 루트에서:

```bash
zeroclaw peripheral flash-nucleo
```

이 명령은 `firmware/nucleo`를 빌드하고 `probe-rs run --chip STM32F401RETx`를 실행합니다. firmware는 flash 직후 즉시 실행됩니다.

### 1.3 수동 Flash (대안)

```bash
cd firmware/nucleo
cargo build --release --target thumbv7em-none-eabihf
probe-rs run --chip STM32F401RETx target/thumbv7em-none-eabihf/release/nucleo
```

---

## 2단계: 시리얼 포트 찾기

- **macOS:** `/dev/cu.usbmodem*` 또는 `/dev/tty.usbmodem*` (예: `/dev/cu.usbmodem101`)
- **Linux:** `/dev/ttyACM0` (또는 연결 후 `dmesg`로 확인)

USART2 (PA2/PA3)가 ST-Link의 가상 COM 포트에 브릿지되어 있어 호스트에서는 하나의 시리얼 장치로 보입니다.

---

## 3단계: ZeroClaw 설정

`~/.zeroclaw/config.toml`에 추가합니다:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/cu.usbmodem101"   # 사용 중인 포트에 맞게 조정
baud = 115200
```

---

## 4단계: 실행 및 테스트

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

또는 에이전트를 직접 사용합니다:

```bash
zeroclaw agent --message "Turn on the LED on pin 13"
```

핀 13 = PA5 = Nucleo-F401RE의 사용자 LED (LD2)입니다.

---

## 요약: 명령어

| 단계 | 명령어 |
|------|---------|
| 1 | Nucleo를 USB로 연결 |
| 2 | `cargo install probe-rs-tools --locked` |
| 3 | `zeroclaw peripheral flash-nucleo` |
| 4 | config.toml에 Nucleo 추가 (path = 시리얼 포트) |
| 5 | `zeroclaw daemon` 또는 `zeroclaw agent -m "Turn on LED"` |

---

## 문제 해결

- **flash-nucleo 인식 불가** — 저장소에서 빌드합니다: `cargo run --features hardware -- peripheral flash-nucleo`. 이 서브커맨드는 저장소 빌드에만 포함되며, crates.io 설치에는 포함되지 않습니다.
- **probe-rs를 찾을 수 없음** — `cargo install probe-rs-tools --locked` (`probe-rs` crate는 라이브러리이며, CLI는 `probe-rs-tools`에 있습니다)
- **프로브 감지 불가** — Nucleo가 연결되어 있는지 확인합니다. 다른 USB 케이블/포트를 시도합니다.
- **시리얼 포트를 찾을 수 없음** — Linux에서는 사용자를 `dialout` 그룹에 추가합니다: `sudo usermod -a -G dialout $USER`, 그 후 로그아웃/로그인합니다.
- **GPIO 명령이 무시됨** — 설정의 `path`가 시리얼 포트와 일치하는지 확인합니다. `zeroclaw peripheral list`를 실행하여 확인합니다.
