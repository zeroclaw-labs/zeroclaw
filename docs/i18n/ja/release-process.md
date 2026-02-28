# ローカライズブリッジ: Release Process

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../release-process.md](../../release-process.md)

## テーマ位置付け

- 分類: エンジニアリング運用
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · Release Goals](../../release-process.md#release-goals)
- [H2 · Standard Cadence](../../release-process.md#standard-cadence)
- [H2 · Workflow Contract](../../release-process.md#workflow-contract)
- [H2 · Maintainer Procedure](../../release-process.md#maintainer-procedure)
- [H3 · 1) Preflight on `main`](../../release-process.md#1-preflight-on-main)
- [H3 · 2) Run verification build (no publish)](../../release-process.md#2-run-verification-build-no-publish)
- [H3 · 3) Cut release tag](../../release-process.md#3-cut-release-tag)
- [H3 · 4) Monitor publish run](../../release-process.md#4-monitor-publish-run)
- [H3 · 5) Post-release validation](../../release-process.md#5-post-release-validation)
- [H3 · 6) Publish Homebrew Core formula (bot-owned)](../../release-process.md#6-publish-homebrew-core-formula-bot-owned)
- [H2 · Emergency / Recovery Path](../../release-process.md#emergency-recovery-path)
- [H2 · Operational Notes](../../release-process.md#operational-notes)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
