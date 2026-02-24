# ローカライズブリッジ: Matrix E2ee Guide

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../matrix-e2ee-guide.md](../../matrix-e2ee-guide.md)

## テーマ位置付け

- 分類: ランタイムと接続
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · 0. Fast FAQ (#499-class symptom)](../../matrix-e2ee-guide.md#0-fast-faq-499-class-symptom)
- [H2 · 1. Requirements](../../matrix-e2ee-guide.md#1-requirements)
- [H2 · 2. Configuration](../../matrix-e2ee-guide.md#2-configuration)
- [H3 · About `user_id` and `device_id`](../../matrix-e2ee-guide.md#about-user-id-and-device-id)
- [H2 · 3. Quick Validation Flow](../../matrix-e2ee-guide.md#3-quick-validation-flow)
- [H2 · 4. Troubleshooting “No Response”](../../matrix-e2ee-guide.md#4-troubleshooting-no-response)
- [H3 · A. Room and membership](../../matrix-e2ee-guide.md#a-room-and-membership)
- [H3 · B. Sender allowlist](../../matrix-e2ee-guide.md#b-sender-allowlist)
- [H3 · C. Token and identity](../../matrix-e2ee-guide.md#c-token-and-identity)
- [H3 · D. E2EE-specific checks](../../matrix-e2ee-guide.md#d-e2ee-specific-checks)
- [H3 · E. Message formatting (Markdown)](../../matrix-e2ee-guide.md#e-message-formatting-markdown)
- [H3 · F. Fresh start test](../../matrix-e2ee-guide.md#f-fresh-start-test)
- [H2 · 5. Operational Notes](../../matrix-e2ee-guide.md#5-operational-notes)
- [H2 · 6. Related Docs](../../matrix-e2ee-guide.md#6-related-docs)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
