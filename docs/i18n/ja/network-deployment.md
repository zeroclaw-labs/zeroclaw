# ローカライズブリッジ: Network Deployment

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../network-deployment.md](../../network-deployment.md)

## テーマ位置付け

- 分類: ランタイムと接続
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · 1. Overview](../../network-deployment.md#1-overview)
- [H2 · 2. ZeroClaw on Raspberry Pi](../../network-deployment.md#2-zeroclaw-on-raspberry-pi)
- [H3 · 2.1 Prerequisites](../../network-deployment.md#2-1-prerequisites)
- [H3 · 2.2 Install](../../network-deployment.md#2-2-install)
- [H3 · 2.3 Config](../../network-deployment.md#2-3-config)
- [H3 · 2.4 Run Daemon (Local Only)](../../network-deployment.md#2-4-run-daemon-local-only)
- [H2 · 3. Binding to 0.0.0.0 (Local Network)](../../network-deployment.md#3-binding-to-0-0-0-0-local-network)
- [H3 · 3.1 Option A: Explicit Opt-In](../../network-deployment.md#3-1-option-a-explicit-opt-in)
- [H3 · 3.2 Option B: Tunnel (Recommended for Webhooks)](../../network-deployment.md#3-2-option-b-tunnel-recommended-for-webhooks)
- [H2 · 4. Telegram Polling (No Inbound Port)](../../network-deployment.md#4-telegram-polling-no-inbound-port)
- [H3 · 4.1 Single Poller Rule (Important)](../../network-deployment.md#4-1-single-poller-rule-important)
- [H2 · 5. Webhook Channels (WhatsApp, Nextcloud Talk, Custom)](../../network-deployment.md#5-webhook-channels-whatsapp-nextcloud-talk-custom)
- [H3 · 5.1 Tailscale Funnel](../../network-deployment.md#5-1-tailscale-funnel)
- [H3 · 5.2 ngrok](../../network-deployment.md#5-2-ngrok)
- [H3 · 5.3 Cloudflare Tunnel](../../network-deployment.md#5-3-cloudflare-tunnel)
- [H2 · 6. Checklist: RPi Deployment](../../network-deployment.md#6-checklist-rpi-deployment)
- [H2 · 7. OpenRC (Alpine Linux Service)](../../network-deployment.md#7-openrc-alpine-linux-service)
- [H3 · 7.1 Prerequisites](../../network-deployment.md#7-1-prerequisites)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
