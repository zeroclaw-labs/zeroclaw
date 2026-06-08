# Adding Boards and Tools — ZeroClaw Hardware Guide

This guide explains how to add new hardware boards and custom tools to ZeroClaw.

## Quick Start: Add a Board via CLI

```bash
# Add a board (updates ~/.zeroclaw/config.toml)
zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
zeroclaw peripheral add arduino-uno /dev/cu.usbmodem12345
zeroclaw peripheral add rpi-gpio native   # for Raspberry Pi GPIO (Linux)

# Restart daemon to apply
zeroclaw daemon --host 127.0.0.1 --port 42617
```

## Supported Boards

| Board           | Transport | Path Example              |
|-----------------|-----------|---------------------------|
| nucleo-f401re   | serial    | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno     | serial    | /dev/ttyACM0, /dev/cu.usbmodem* |
| arduino-uno-q   | bridge    | (Uno Q IP)                |
| rpi-gpio        | native    | native                    |
| esp32           | serial    | /dev/ttyUSB0              |

## Manual Config

Boards are configured under `[peripherals]` and `[[peripherals.boards]]` in `~/.zeroclaw/config.toml`. See the [Config reference](../reference/config.md) for the full field index, including `datasheet_dir` (RAG source).

## Adding a Datasheet (RAG)

Place `.md` or `.txt` files in `docs/datasheets/` (or your `datasheet_dir`). Name files by board: `nucleo-f401re.md`, `arduino-uno.md`.

### Pin Aliases (Recommended)

Add a `## Pin Aliases` section so the agent can map "red led" → pin 13:

```markdown
# My Board

## Pin Aliases

| alias       | pin |
|-------------|-----|
| red_led     | 13  |
| builtin_led | 13  |
| user_led    | 5   |
```

Or use key-value format:

```markdown
## Pin Aliases
red_led: 13
builtin_led: 13
```

### PDF Datasheets

With the `rag-pdf` feature, ZeroClaw can index PDF files:

```bash
cargo build --features hardware,rag-pdf
```

Place PDFs in the datasheet directory. They are extracted and chunked for RAG.

## Adding a New Board Type

1. **Create a datasheet** — `docs/datasheets/my-board.md` with pin aliases and GPIO info.
2. **Add to config** — `zeroclaw peripheral add my-board /dev/ttyUSB0`
3. **Implement a peripheral** (optional) — For custom protocols, implement the `Peripheral` trait in `crates/zeroclaw-hardware/src/peripherals/` and register in `create_peripheral_tools`.

See [`docs/hardware/hardware-peripherals-design.md`](../hardware/hardware-peripherals-design.md) for the full design.

## Adding a Custom Tool

1. Implement the `Tool` trait in `crates/zeroclaw-tools/src/`.
2. Register in `create_peripheral_tools` (for hardware tools) or the agent tool registry.
3. Add a tool description to the agent's `tool_descs` in `crates/zeroclaw-runtime/src/agent/loop_.rs`.

## CLI Reference

See the [generated CLI reference](../reference/cli.md) for `zeroclaw peripheral` and `zeroclaw hardware` subcommands.

## Troubleshooting

- **Serial port not found** — On macOS use `/dev/cu.usbmodem*`; on Linux use `/dev/ttyACM0` or `/dev/ttyUSB0`.
- **Build with hardware** — `cargo build --features hardware`
- **Probe-rs for Nucleo** — `cargo build --features hardware,probe`
