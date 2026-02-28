# ローカライズブリッジ: Arduino Uno Q Setup

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../arduino-uno-q-setup.md](../../arduino-uno-q-setup.md)

## テーマ位置付け

- 分類: ハードウェアと周辺機器
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · What's Included (No Code Changes Needed)](../../arduino-uno-q-setup.md#what-s-included-no-code-changes-needed)
- [H2 · Prerequisites](../../arduino-uno-q-setup.md#prerequisites)
- [H2 · Phase 1: Initial Uno Q Setup (One-Time)](../../arduino-uno-q-setup.md#phase-1-initial-uno-q-setup-one-time)
- [H3 · 1.1 Configure Uno Q via App Lab](../../arduino-uno-q-setup.md#1-1-configure-uno-q-via-app-lab)
- [H3 · 1.2 Verify SSH Access](../../arduino-uno-q-setup.md#1-2-verify-ssh-access)
- [H2 · Phase 2: Install ZeroClaw on Uno Q](../../arduino-uno-q-setup.md#phase-2-install-zeroclaw-on-uno-q)
- [H3 · Option A: Build on the Device (Simpler, ~20–40 min)](../../arduino-uno-q-setup.md#option-a-build-on-the-device-simpler-20-40-min)
- [H3 · Option B: Cross-Compile on Mac (Faster)](../../arduino-uno-q-setup.md#option-b-cross-compile-on-mac-faster)
- [H2 · Phase 3: Configure ZeroClaw](../../arduino-uno-q-setup.md#phase-3-configure-zeroclaw)
- [H3 · 3.1 Run Onboard (or Create Config Manually)](../../arduino-uno-q-setup.md#3-1-run-onboard-or-create-config-manually)
- [H3 · 3.2 Minimal config.toml](../../arduino-uno-q-setup.md#3-2-minimal-config-toml)
- [H2 · Phase 4: Run ZeroClaw Daemon](../../arduino-uno-q-setup.md#phase-4-run-zeroclaw-daemon)
- [H2 · Phase 5: GPIO via Bridge (ZeroClaw Handles It)](../../arduino-uno-q-setup.md#phase-5-gpio-via-bridge-zeroclaw-handles-it)
- [H3 · 5.1 Deploy Bridge App](../../arduino-uno-q-setup.md#5-1-deploy-bridge-app)
- [H3 · 5.2 Add to config.toml](../../arduino-uno-q-setup.md#5-2-add-to-config-toml)
- [H3 · 5.3 Run ZeroClaw](../../arduino-uno-q-setup.md#5-3-run-zeroclaw)
- [H2 · Summary: Commands Start to End](../../arduino-uno-q-setup.md#summary-commands-start-to-end)
- [H2 · Troubleshooting](../../arduino-uno-q-setup.md#troubleshooting)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
