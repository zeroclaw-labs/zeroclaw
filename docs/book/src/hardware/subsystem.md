# Hardware subsystem

ZeroClaw's hardware subsystem lets the agent control microcontrollers, SBCs, and peripherals directly. Enable with `--features hardware`.

## What's supported

The hardware subsystem identifies boards by USB VID/PID. The boards in the
canonical registry:

{{#include ../_snippets/hardware-boards.md}}

Transports the subsystem speaks:

{{#include ../_snippets/hardware-transports.md}}

See [Peripherals design](./hardware-peripherals-design.md) for the architecture
and the per-board setup guides ([Nucleo](./nucleo-setup.md),
[Arduino Uno Q](./arduino-uno-q-setup.md), [Aardvark](./aardvark.md),
[Raspberry Pi](./raspberry-pi-setup.md), [Android](./android-setup.md)) for
wiring each one up.

## Enabling

At compile time:

<div class="os-tabs-src">

#### sh

```sh
cargo build --release --features hardware
```

</div>

The hardware features are `hardware` (core subsystem), `peripheral-rpi`
(Raspberry Pi native GPIO), and `probe` (probe-rs SWD introspection). See the
[Config reference](../reference/config.md) for the per-board config fields.

## Runtime tools

With the `hardware` feature, the agent gains these built-in tools:

{{#include ../_snippets/hardware-tools-base.md}}

When an Aardvark adapter is connected at startup, these additional tools load:

{{#include ../_snippets/hardware-tools-aardvark.md}}

All tool invocations go through the same [security policy](../security/overview.md) as any other tool. Hardware tools only reach the device paths explicitly listed in `[[peripherals.boards]]` entries:

## Running on a Raspberry Pi

The most common hardware target. A minimal setup:

<div class="os-tabs-src">

#### sh

```sh
# install
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash

# add yourself to hardware groups (re-login after)
sudo usermod -aG gpio,spi,i2c $USER

# install as user service (ensures hardware group membership is inherited)
zeroclaw service install
```

</div>

The stock systemd unit sets `SupplementaryGroups=gpio spi i2c`.

## Safety

Hardware tools can brick things. Real, expensive things.

- `pico_flash` writes firmware; a bad image can brick the board. The tool requires operator approval at `Supervised` autonomy regardless of autonomy level; there's no way to auto-approve it.
- `i2c_write` / `spi_transfer` to device addresses the agent doesn't know can damage sensors.
- GPIO writes that conflict with external drivers (voltage fights) damage pins.

For production deployments with untrusted channels exposed, keep hardware tools off non-CLI channels via the global `autonomy.non_cli_excluded_tools` list (the schema has no per-channel `tools_deny` field). Tools listed there are omitted from the tool specs sent to the model on every non-CLI channel (Discord, Telegram, Bluesky, etc.). The local CLI still sees them.

## Datasheets

Per-board pin maps and electrical characteristics:

- STM32 Nucleo-F401RE: <https://www.st.com/en/evaluation-tools/nucleo-f401re.html>
- Arduino Uno Q: <https://docs.arduino.cc/hardware/uno-q>
- Raspberry Pi GPIO: <https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#gpio>
- ESP32: <https://www.espressif.com/sites/default/files/documentation/esp32_datasheet_en.pdf>
