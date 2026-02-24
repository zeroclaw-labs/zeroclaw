# Локализованный bridge: Hardware Peripherals Design

Это усиленная bridge-страница. Здесь собраны позиционирование темы, навигация по разделам оригинала и практические подсказки.

Английский оригинал:

- [../../hardware-peripherals-design.md](../../hardware-peripherals-design.md)

## Позиционирование темы

- Категория: Hardware и периферия
- Глубина: усиленный bridge (карта разделов + операционные подсказки)
- Применение: сначала понять структуру, затем выполнять по английскому нормативному описанию.

## Карта разделов оригинала

- [H2 · 1. Vision](../../hardware-peripherals-design.md#1-vision)
- [H2 · 2. Two Modes of Operation](../../hardware-peripherals-design.md#2-two-modes-of-operation)
- [H3 · Mode 1: Edge-Native (Standalone)](../../hardware-peripherals-design.md#mode-1-edge-native-standalone)
- [H3 · Mode 2: Host-Mediated (Development / Debugging)](../../hardware-peripherals-design.md#mode-2-host-mediated-development-debugging)
- [H3 · Mode Comparison](../../hardware-peripherals-design.md#mode-comparison)
- [H2 · 3. Legacy / Simpler Modes (Pre-LLM-on-Edge)](../../hardware-peripherals-design.md#3-legacy-simpler-modes-pre-llm-on-edge)
- [H3 · Mode A: Host + Remote Peripheral (STM32 via serial)](../../hardware-peripherals-design.md#mode-a-host-remote-peripheral-stm32-via-serial)
- [H3 · Mode B: RPi as Host (Native GPIO)](../../hardware-peripherals-design.md#mode-b-rpi-as-host-native-gpio)
- [H2 · 4. Technical Requirements](../../hardware-peripherals-design.md#4-technical-requirements)
- [H3 · RAG Pipeline (Datasheet Retrieval)](../../hardware-peripherals-design.md#rag-pipeline-datasheet-retrieval)
- [H3 · Dynamic Execution Options](../../hardware-peripherals-design.md#dynamic-execution-options)
- [H2 · 5. CLI and Config](../../hardware-peripherals-design.md#5-cli-and-config)
- [H3 · CLI Flags](../../hardware-peripherals-design.md#cli-flags)
- [H3 · Config (config.toml)](../../hardware-peripherals-design.md#config-config-toml)
- [H2 · 6. Architecture: Peripheral as Extension Point](../../hardware-peripherals-design.md#6-architecture-peripheral-as-extension-point)
- [H3 · New Trait: `Peripheral`](../../hardware-peripherals-design.md#new-trait-peripheral)
- [H3 · Flow](../../hardware-peripherals-design.md#flow)
- [H3 · Board Support](../../hardware-peripherals-design.md#board-support)

## Практические рекомендации

- Сначала просмотрите структуру разделов оригинала, затем переходите к релевантным блокам для текущего изменения.
- Имена команд, ключей конфигурации, API-пути и code identifiers оставляйте на английском.
- При расхождениях трактовки опирайтесь на английский оригинал.

## Связанные входы

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
