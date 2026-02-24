# ローカライズブリッジ: Resource Limits

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../resource-limits.md](../../resource-limits.md)

## テーマ位置付け

- 分類: セキュリティと統制
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · Problem](../../resource-limits.md#problem)
- [H2 · Proposed Solutions](../../resource-limits.md#proposed-solutions)
- [H3 · Option 1: cgroups v2 (Linux, Recommended)](../../resource-limits.md#option-1-cgroups-v2-linux-recommended)
- [H3 · Option 2: tokio::task::deadlock detection](../../resource-limits.md#option-2-tokio-task-deadlock-detection)
- [H3 · Option 3: Memory monitoring](../../resource-limits.md#option-3-memory-monitoring)
- [H2 · Config Schema](../../resource-limits.md#config-schema)
- [H2 · Implementation Priority](../../resource-limits.md#implementation-priority)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
