# 보드 및 도구 추가 — ZeroClaw 하드웨어 가이드

이 가이드는 ZeroClaw에 새로운 하드웨어 보드와 커스텀 도구를 추가하는 방법을 설명합니다.

## 빠른 시작: CLI로 보드 추가

```bash
# 보드 추가 (~/.zeroclaw/config.toml 업데이트)
zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
zeroclaw peripheral add arduino-uno /dev/cu.usbmodem12345
zeroclaw peripheral add rpi-gpio native   # Raspberry Pi GPIO용 (Linux)

# 적용을 위해 데몬 재시작
zeroclaw daemon --host 127.0.0.1 --port 42617
```

## 지원 보드

| 보드           | 전송 방식 | 경로 예시              |
|-----------------|-----------|---------------------------|
| nucleo-f401re   | serial    | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno     | serial    | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno-q   | bridge    | (Uno Q IP)                |
| rpi-gpio        | native    | native                    |
| esp32           | serial    | /dev/ttyUSB0              |

## 수동 설정

`~/.zeroclaw/config.toml`을 편집합니다:

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets" # 선택 사항: "빨간 LED 켜기" → pin 13을 위한 RAG

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

## 데이터시트 추가 (RAG)

`docs/datasheets/` (또는 `datasheet_dir`)에 `.md` 또는 `.txt` 파일을 배치합니다. 파일 이름은 보드별로 지정합니다: `nucleo-f401re.md`, `arduino-uno.md`.

### Pin 별칭 (권장)

에이전트가 "빨간 LED" -> pin 13으로 매핑할 수 있도록 `## Pin Aliases` 섹션을 추가합니다:

```markdown
# My Board

## Pin Aliases

| alias       | pin |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 5   |
```

또는 키-값 형식을 사용합니다:

```markdown
## Pin Aliases
red_led: 13
builtin_led: 13
```

### PDF 데이터시트

`rag-pdf` 기능으로 ZeroClaw는 PDF 파일을 인덱싱할 수 있습니다:

```bash
cargo build --features hardware,rag-pdf
```

데이터시트 디렉토리에 PDF를 배치합니다. RAG를 위해 추출 및 청킹됩니다.

## 새 보드 유형 추가

1. **데이터시트 생성** — pin 별칭과 GPIO 정보가 포함된 `docs/datasheets/my-board.md`.
2. **설정에 추가** — `zeroclaw peripheral add my-board /dev/ttyUSB0`
3. **주변기기 구현** (선택 사항) — 커스텀 프로토콜의 경우, `src/peripherals/`에서 `Peripheral` trait을 구현하고 `create_peripheral_tools`에 등록합니다.

전체 설계는 [`docs/hardware/hardware-peripherals-design.md`](../hardware/hardware-peripherals-design.md)를 참조합니다.

## 커스텀 도구 추가

1. `src/tools/`에서 `Tool` trait을 구현합니다.
2. `create_peripheral_tools` (하드웨어 도구용) 또는 에이전트 도구 레지스트리에 등록합니다.
3. `src/agent/loop_.rs`의 에이전트 `tool_descs`에 도구 설명을 추가합니다.

## CLI 참조

| 명령 | 설명 |
|---------|-------------|
| `zeroclaw peripheral list` | 설정된 보드 목록 |
| `zeroclaw peripheral add <board> <path>` | 보드 추가 (설정 파일에 기록) |
| `zeroclaw peripheral flash` | Arduino 펌웨어 플래시 |
| `zeroclaw peripheral flash-nucleo` | Nucleo 펌웨어 플래시 |
| `zeroclaw hardware discover` | USB 디바이스 목록 |
| `zeroclaw hardware info` | probe-rs를 통한 칩 정보 |

## 문제 해결

- **시리얼 포트를 찾을 수 없음** — macOS에서는 `/dev/cu.usbmodem*`을 사용하고, Linux에서는 `/dev/ttyACM0` 또는 `/dev/ttyUSB0`을 사용합니다.
- **하드웨어 빌드** — `cargo build --features hardware`
- **Nucleo용 Probe-rs** — `cargo build --features hardware,probe`
