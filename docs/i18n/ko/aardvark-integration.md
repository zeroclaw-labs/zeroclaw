# Aardvark 통합 -- 작동 방식

각 구성 요소와 그 연결 방식에 대한 평이한 설명입니다.

---

## 전체 구조

```
┌──────────────────────────────────────────────────────────────┐
│                        STARTUP (boot)                        │
│                                                              │
│  1. Ask aardvark-sys: "any adapters plugged in?"            │
│  2. For each one found → register a device + transport       │
│  3. Load tools only if hardware was found                    │
└──────────────────────────────────────────────┬───────────────┘
                                           │
                    ┌──────────────────────▼──────────────────────┐
                    │              RUNTIME (agent loop)            │
                    │                                              │
                    │  User: "scan i2c bus"                        │
                    │     → agent calls i2c_scan tool              │
                    │     → tool builds a ZcCommand                │
                    │     → AardvarkTransport sends to hardware     │
                    │     → response flows back as text            │
                    └──────────────────────────────────────────────┘
```

---

## 계층별 설명

### 계층 1 -- `aardvark-sys` (USB 통신 담당)

**파일:** `crates/aardvark-sys/src/lib.rs`

원시 C 라이브러리와 직접 통신하는 유일한 계층입니다.
C 함수 호출을 안전한 Rust로 변환하는 얇은 번역기라고 생각하시면 됩니다.

**알고리즘:**

```
find_devices()
  → call aa_find_devices(16, buf)       // C 라이브러리에 어댑터 수 문의
  → return Vec of port numbers          // [0, 1, ...] 어댑터당 하나

open_port(port)
  → call aa_open(port)                  // 해당 어댑터 열기
  → if handle ≤ 0, return OpenFailed
  → else return AardvarkHandle{ _port: handle }

i2c_scan(handle)
  → for addr in 0x08..=0x77            // 유효한 모든 7비트 주소
      try aa_i2c_read(addr, 1 byte)    // 응답 확인
      if ACK → add to list             // 장치가 응답함
  → return list of live addresses

i2c_read(handle, addr, len)
  → aa_i2c_read(addr, len bytes)
  → return bytes as Vec<u8>

i2c_write(handle, addr, data)
  → aa_i2c_write(addr, data)

spi_transfer(handle, bytes_to_send)
  → aa_spi_write(bytes)                // 전이중: 송신 + 수신
  → return received bytes

gpio_set(handle, direction, value)
  → aa_gpio_direction(direction)       // 출력 핀 지정
  → aa_gpio_put(value)                 // 출력 레벨 설정

gpio_get(handle)
  → aa_gpio_get()                      // 모든 핀 레벨을 비트마스크로 읽기

Drop(handle)
  → aa_close(handle._port)            // drop 시 항상 닫기
```

**스텁 모드** (SDK 없음): 모든 메서드가 즉시 `Err(NotFound)`를 반환합니다. `find_devices()`는 `[]`를 반환합니다. 크래시가 발생하지 않습니다.

---

### 계층 2 -- `AardvarkTransport` (브릿지)

**파일:** `src/hardware/aardvark.rs`

ZeroClaw의 나머지 부분은 하나의 언어로 통신합니다: `ZcCommand` -> `ZcResponse`.
`AardvarkTransport`는 이 프로토콜과 위의 aardvark-sys 호출 사이를 변환합니다.

**알고리즘:**

```
send(ZcCommand) → ZcResponse

  extract command name from cmd.name
  extract parameters from cmd.params (serde_json values)

  match cmd.name:

    "i2c_scan"   → open handle → call i2c_scan()
                   → format found addresses as hex list
                   → return ZcResponse{ output: "0x48, 0x68" }

    "i2c_read"   → parse addr (hex string) + len (number)
                   → open handle → i2c_enable(bitrate)
                   → call i2c_read(addr, len)
                   → format bytes as hex
                   → return ZcResponse{ output: "0xAB 0xCD" }

    "i2c_write"  → parse addr + data bytes
                   → open handle → i2c_write(addr, data)
                   → return ZcResponse{ output: "ok" }

    "spi_transfer" → parse bytes_hex string → decode to Vec<u8>
                     → open handle → spi_enable(bitrate)
                     → spi_transfer(bytes)
                     → return received bytes as hex

    "gpio_set"   → parse direction + value bitmasks
                   → open handle → gpio_set(dir, val)
                   → return ZcResponse{ output: "ok" }

    "gpio_get"   → open handle → gpio_get()
                   → return bitmask value as string

  on any AardvarkError → return ZcResponse{ error: "..." }
```

**핵심 설계 선택 -- lazy open:** 핸들은 각 명령마다 새로 열리고 명령 종료 시 drop됩니다. 이는 유지되는 연결이 없고, 정리할 상태가 없으며, "아직 열려 있나?"라는 로직이 어디에도 필요 없다는 것을 의미합니다.

---

### 계층 3 -- Tools (에이전트가 호출하는 것)

**파일:** `src/hardware/aardvark_tools.rs`

각 tool은 얇은 래퍼입니다. 다음을 수행합니다:
1. 에이전트의 JSON 입력을 검증합니다
2. 사용할 물리적 장치를 결정합니다
3. `ZcCommand`를 구성합니다
4. `AardvarkTransport.send()`를 호출합니다
5. 결과를 텍스트로 반환합니다

```
I2cScanTool.call(args)
  → look up "device" in args (default: "aardvark0")
  → find that device in the registry
  → build ZcCommand{ name: "i2c_scan", params: {} }
  → send to AardvarkTransport
  → return "Found: 0x48, 0x68" (or "No devices found")

I2cReadTool.call(args)
  → require args["addr"] and args["len"]
  → build ZcCommand{ name: "i2c_read", params: {addr, len} }
  → send → return hex bytes

I2cWriteTool.call(args)
  → require args["addr"] and args["data"] (hex or array)
  → build ZcCommand{ name: "i2c_write", params: {addr, data} }
  → send → return "ok" or error

SpiTransferTool.call(args)
  → require args["bytes"] (hex string)
  → build ZcCommand{ name: "spi_transfer", params: {bytes} }
  → send → return received bytes

GpioAardvarkTool.call(args)
  → require args["direction"] + args["value"]  (set)
         OR no extra args                       (get)
  → build appropriate ZcCommand
  → send → return result

DatasheetTool.call(args)
  → action = args["action"]: "search" | "download" | "list" | "read"
  → "search":   return a Google/vendor search URL for the device
  → "download": fetch PDF from args["url"] → save to ~/.zeroclaw/hardware/datasheets/
  → "list":     scan the datasheets directory → return filenames
  → "read":     open a saved PDF and return its text
```

---

### 계층 4 -- Device Registry (주소록)

**파일:** `src/hardware/device.rs`

레지스트리는 연결된 모든 장치의 런타임 맵입니다.
각 항목은 별칭, 종류, 기능, transport 핸들을 저장합니다.

```
register("aardvark", vid=0x2b76, ...)
  → DeviceKind::from_vid(0x2b76)  → DeviceKind::Aardvark
  → DeviceRuntime::from_kind()    → DeviceRuntime::Aardvark
  → assign alias "aardvark0" (then "aardvark1" for second, etc.)
  → store entry in HashMap

attach_transport("aardvark0", AardvarkTransport, capabilities{i2c,spi,gpio})
  → store Arc<dyn Transport> in the entry

has_aardvark()
  → any entry where kind == Aardvark  → true / false

resolve_aardvark_device(args)
  → read "device" param (default: "aardvark0")
  → look up alias in HashMap
  → return (alias, DeviceContext{ transport, capabilities })
```

---

### 계층 5 -- `boot()` (시작 시 연결)

**파일:** `src/hardware/mod.rs`

`boot()`는 시작 시 한 번 실행됩니다. Aardvark의 경우:

```
boot()
  ...
  aardvark_ports = aardvark_sys::AardvarkHandle::find_devices()
  // → [] in stub mode, [0] if one adapter is plugged in

  for (i, port) in aardvark_ports:
    alias = registry.register("aardvark", vid=0x2b76, ...)
    // → "aardvark0", "aardvark1", ...

    transport = AardvarkTransport::new(port, bitrate=100kHz)
    registry.attach_transport(alias, transport, {i2c:true, spi:true, gpio:true})

    log "[registry] aardvark0 ready → Total Phase port 0"
  ...
```

---

### 계층 6 -- Tool Registry (로더)

**파일:** `src/hardware/tool_registry.rs`

`boot()` 이후, tool 레지스트리는 어떤 하드웨어가 있는지 확인하고
관련 tool만 로드합니다:

```
ToolRegistry::load(devices)

  # 항상 로드됨 (Pico / GPIO)
  register: gpio_write, gpio_read, gpio_toggle, pico_flash, device_list, device_status

  # boot 시 Aardvark가 발견된 경우에만 로드됨
  if devices.has_aardvark():
    register: i2c_scan, i2c_read, i2c_write, spi_transfer, gpio_aardvark, datasheet
```

이것이 `hardware_feature_registers_all_six_tools` 테스트가 스텁 모드에서도 통과하는 이유입니다. `has_aardvark()`가 false를 반환하면 추가 tool이 로드되지 않아 개수가 6개로 유지됩니다.

---

## 전체 흐름 다이어그램

```
 SDK FILES          aardvark-sys            ZeroClaw core
 (vendor/)          (crates/)               (src/)
─────────────────────────────────────────────────────────────────

 aardvark.h  ──►  build.rs         boot()
 aardvark.so       (bindgen)    ──►  find_devices()
                       │                │
                  bindings.rs           │  vec![0]  (one adapter)
                       │                ▼
                  lib.rs           register("aardvark0")
                  AardvarkHandle   attach_transport(AardvarkTransport)
                       │                │
                       │                ▼
                       │         ToolRegistry::load()
                       │           has_aardvark() == true
                       │           → load 6 aardvark tools
                       │
─────────────────────────────────────────────────────────────────

 USER MESSAGE: "scan the i2c bus"

  agent loop
      │
      ▼
  I2cScanTool.call()
      │
      ▼
  resolve_aardvark_device("aardvark0")
      │  returns transport Arc
      ▼
  AardvarkTransport.send(ZcCommand{ name: "i2c_scan" })
      │
      ▼
  AardvarkHandle::open_port(0)    ← opens USB connection
      │
      ▼
  aa_i2c_read(0x08..0x77)         ← probes each address
      │
      ▼
  AardvarkHandle dropped           ← USB connection closed
      │
      ▼
  ZcResponse{ output: "Found: 0x48, 0x68" }
      │
      ▼
  agent sends reply to user: "I found two I2C devices: 0x48 and 0x68"
```

---

## 스텁 모드 vs 실제 하드웨어 비교

| | 스텁 모드 (현재) | 실제 하드웨어 |
|---|---|---|
| `find_devices()` | `[]` 반환 | `[0]` 반환 |
| `open_port(0)` | `Err(NotFound)` | USB 열기, 핸들 반환 |
| `i2c_scan()` | `[]` | 버스 탐색, 주소 반환 |
| 로드된 tools | Pico tool 6개만 | Pico 6개 + Aardvark 6개 |
| `has_aardvark()` | `false` | `true` |
| SDK 필요 여부 | 아니오 | 예 (`vendor/aardvark.h` + `.so`) |

실제 하드웨어를 연결했을 때 변경되는 코드는 `crates/aardvark-sys/src/lib.rs` 내부뿐입니다. 다른 모든 계층은 이미 연결되어 대기 중입니다.
