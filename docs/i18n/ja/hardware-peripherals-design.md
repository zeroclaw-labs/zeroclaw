# ローカライズブリッジ: Hardware Peripherals Design

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../hardware-peripherals-design.md](../../hardware-peripherals-design.md)

## テーマ位置付け

- 分類: ハードウェアと周辺機器
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

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

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
