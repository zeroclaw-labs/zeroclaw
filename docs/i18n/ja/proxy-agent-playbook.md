# ローカライズブリッジ: Proxy Agent Playbook

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../proxy-agent-playbook.md](../../proxy-agent-playbook.md)

## テーマ位置付け

- 分類: Provider と統合
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · 0. Summary](../../proxy-agent-playbook.md#0-summary)
- [H2 · 1. Fast Path by Intent](../../proxy-agent-playbook.md#1-fast-path-by-intent)
- [H3 · 1.1 Proxy only ZeroClaw internal traffic](../../proxy-agent-playbook.md#1-1-proxy-only-zeroclaw-internal-traffic)
- [H3 · 1.2 Proxy only selected services](../../proxy-agent-playbook.md#1-2-proxy-only-selected-services)
- [H3 · 1.3 Export process-wide proxy environment variables](../../proxy-agent-playbook.md#1-3-export-process-wide-proxy-environment-variables)
- [H3 · 1.4 Emergency rollback](../../proxy-agent-playbook.md#1-4-emergency-rollback)
- [H2 · 2. Scope Decision Matrix](../../proxy-agent-playbook.md#2-scope-decision-matrix)
- [H2 · 3. Standard Safe Workflow](../../proxy-agent-playbook.md#3-standard-safe-workflow)
- [H2 · 4. Mode A — Proxy Only for ZeroClaw Internals](../../proxy-agent-playbook.md#4-mode-a-proxy-only-for-zeroclaw-internals)
- [H2 · 5. Mode B — Proxy Only for Specific Services](../../proxy-agent-playbook.md#5-mode-b-proxy-only-for-specific-services)
- [H3 · 5.1 Target specific services](../../proxy-agent-playbook.md#5-1-target-specific-services)
- [H3 · 5.2 Target by selectors](../../proxy-agent-playbook.md#5-2-target-by-selectors)
- [H2 · 6. Mode C — Proxy for Full Process Environment](../../proxy-agent-playbook.md#6-mode-c-proxy-for-full-process-environment)
- [H3 · 6.1 Configure and apply environment scope](../../proxy-agent-playbook.md#6-1-configure-and-apply-environment-scope)
- [H2 · 7. Disable / Rollback Patterns](../../proxy-agent-playbook.md#7-disable-rollback-patterns)
- [H3 · 7.1 Disable proxy (default safe behavior)](../../proxy-agent-playbook.md#7-1-disable-proxy-default-safe-behavior)
- [H3 · 7.2 Disable proxy and force-clear env vars](../../proxy-agent-playbook.md#7-2-disable-proxy-and-force-clear-env-vars)
- [H3 · 7.3 Keep proxy enabled but clear environment exports only](../../proxy-agent-playbook.md#7-3-keep-proxy-enabled-but-clear-environment-exports-only)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
