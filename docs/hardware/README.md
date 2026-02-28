# Hardware & Peripherals Docs

For board integration, firmware flow, and peripheral architecture.

ZeroClaw's hardware subsystem enables direct control of microcontrollers and peripherals via the `Peripheral` trait. Each board exposes tools for GPIO, ADC, and sensor operations, allowing agent-driven hardware interaction on boards like STM32 Nucleo, Raspberry Pi, and ESP32. See [hardware-peripherals-design.md](../hardware-peripherals-design.md) for the full architecture.

## Entry Points

- Architecture and peripheral model: [../hardware-peripherals-design.md](../hardware-peripherals-design.md)
- Add a new board/tool: [../adding-boards-and-tools.md](../adding-boards-and-tools.md)
- Nucleo setup: [../nucleo-setup.md](../nucleo-setup.md)
- Arduino Uno R4 WiFi setup: [../arduino-uno-q-setup.md](../arduino-uno-q-setup.md)

## Datasheets

- Datasheet index: [../datasheets/README.md](../datasheets/README.md)
- STM32 Nucleo-F401RE: [../datasheets/nucleo-f401re.md](../datasheets/nucleo-f401re.md)
- Arduino Uno: [../datasheets/arduino-uno.md](../datasheets/arduino-uno.md)
- ESP32: [../datasheets/esp32.md](../datasheets/esp32.md)
