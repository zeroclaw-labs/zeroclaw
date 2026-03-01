# 在地化橋接檔案：Hardware Peripherals Design

這是增強型 bridge 頁面。它提供該主題的定位、原文章節導覽和執行提示，使用說明你在不丟失英文規範語義的情況下快速落地。

英文原文:

- [../../hardware-peripherals-design.md](../../hardware-peripherals-design.md)

## 主題定位

- 類別：硬體與外設
- 深度：增強 bridge（章節導覽 + 執行提示）
- 適用：先理解結構，再按英文規範逐條執行。

## 原文章節導覽

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

## 操作建議

- 先通讀原文目錄，再聚焦與你當前變更直接相關的小節。
- 指令名、配置鍵、API 路徑和程式碼標識保持英文。
- 發生語義歧義或行為衝突時，以英文原文為準。

## 相關入口

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
