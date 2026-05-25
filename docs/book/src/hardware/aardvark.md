# Aardvark

A plain-language walkthrough of every piece and how they connect.

---

## The Big Picture

```
┌──────────────────────────────────────────────────────────────┐
│                        STARTUP (boot)                        │
│                                                              │
│  1. Ask aardvark-sys: "any adapters plugged in?"            │
│  2. For each one found → register a device + transport       │
│  3. Load tools only if hardware was found                    │
└──────────────────────────────────────────┬───────────────────┘
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

## Layer by Layer

### Layer 1 — `aardvark-sys` (the USB talker)

**File:** `crates/aardvark-sys/src/lib.rs`

This is the only layer that ever touches the raw C library.
Think of it as a thin translator: it turns C function calls into safe Rust.

**Algorithm:**

```
find_devices()
  → call aa_find_devices(16, buf)       // ask C lib how many adapters
  → return Vec of port numbers          // [0, 1, ...] one per adapter

open_port(port)
  → call aa_open(port)                  // open that specific adapter
  → if handle ≤ 0, return OpenFailed
  → else return AardvarkHandle{ _port: handle }

i2c_scan(handle)
  → for addr in 0x08..=0x77            // every valid 7-bit address
      try aa_i2c_read(addr, 1 byte)    // knock on the door
      if ACK → add to list             // device answered
  → return list of live addresses

i2c_read(handle, addr, len)
  → aa_i2c_read(addr, len bytes)
  → return bytes as Vec<u8>

i2c_write(handle, addr, data)
  → aa_i2c_write(addr, data)

spi_transfer(handle, bytes_to_send)
  → aa_spi_write(bytes)                // full-duplex: sends + receives
  → return received bytes

gpio_set(handle, direction, value)
  → aa_gpio_direction(direction)       // which pins are outputs
  → aa_gpio_put(value)                 // set output levels

gpio_get(handle)
  → aa_gpio_get()                      // read all pin levels as bitmask

Drop(handle)
  → aa_close(handle._port)            // always close on drop
```

**In stub mode** (no SDK): every method returns `Err(NotFound)` immediately. `find_devices()` returns `[]`. Nothing crashes.

---

### Layer 2 — `AardvarkTransport` (the bridge)

**File:** `crates/zeroclaw-hardware/src/aardvark.rs`

The rest of ZeroClaw speaks a single language: `ZcCommand` → `ZcResponse`.
`AardvarkTransport` translates between that protocol and the aardvark-sys calls above.

**Algorithm:**

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

**Key design choice — lazy open:** The handle is opened fresh for every command and dropped at the end. This means no held connection, no state to clean up, and no "is it still open?" logic anywhere.

---

### Layer 3 — Tools (what the agent calls)

**File:** `crates/zeroclaw-hardware/src/aardvark_tools.rs`

Each tool is a thin wrapper. It:
1. Validates the agent's JSON input
2. Resolves which physical device to use
3. Builds a `ZcCommand`
4. Calls `AardvarkTransport.send()`
5. Returns the result as text

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

### Layer 4 — Device Registry (the address book)

**File:** `crates/zeroclaw-hardware/src/device.rs`

The registry is a runtime map of every connected device.
Each entry stores: alias, kind, capabilities, transport handle.

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

### Layer 5 — `boot()` (startup wiring)

**File:** `crates/zeroclaw-hardware/src/lib.rs`

`boot()` runs once at startup. For Aardvark:

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

### Layer 6 — Tool Registry (the loader)

**File:** `crates/zeroclaw-hardware/src/tool_registry.rs`

After `boot()`, the tool registry checks what hardware is present and loads
only the relevant tools:

```
ToolRegistry::load(devices)

  # always loaded (Pico / GPIO)
  register: gpio_write, gpio_read, gpio_toggle, pico_flash, device_list, device_status

  # only loaded if an Aardvark was found at boot
  if devices.has_aardvark():
    register: i2c_scan, i2c_read, i2c_write, spi_transfer, gpio_aardvark, datasheet
```

This is why the `hardware_feature_registers_all_six_tools` test still passes in stub mode — `has_aardvark()` returns false, 0 extra tools load, count stays at 6.

---

## Full Flow Diagram

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

## Stub vs Real Side by Side

| | Stub mode (now) | Real hardware |
|---|---|---|
| `find_devices()` | returns `[]` | returns `[0]` |
| `open_port(0)` | `Err(NotFound)` | opens USB, returns handle |
| `i2c_scan()` | `[]` | probes bus, returns addresses |
| tools loaded | only the 6 Pico tools | 6 Pico + 6 Aardvark tools |
| `has_aardvark()` | `false` | `true` |
| SDK needed | no | yes (`vendor/aardvark.h` + `.so`) |

The only code that changes when you plug in real hardware is inside
`crates/aardvark-sys/src/lib.rs` — every other layer is already wired up
and waiting.
