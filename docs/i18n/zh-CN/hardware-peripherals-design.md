# 本地化桥接文档：Hardware Peripherals Design

这是增强型 bridge 页面。它提供该主题的定位、原文章节导览和执行提示，帮助你在不丢失英文规范语义的情况下快速落地。

英文原文:

- [../../hardware-peripherals-design.md](../../hardware-peripherals-design.md)

## 主题定位

- 类别：硬件与外设
- 深度：增强 bridge（章节导览 + 执行提示）
- 适用：先理解结构，再按英文规范逐条执行。

## 原文章节导览

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

## 操作建议

- 先通读原文目录，再聚焦与你当前变更直接相关的小节。
- 命令名、配置键、API 路径和代码标识保持英文。
- 发生语义歧义或行为冲突时，以英文原文为准。

## 相关入口

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
