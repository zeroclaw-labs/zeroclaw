# ローカライズブリッジ: Sandboxing

このページは強化版ブリッジです。テーマの位置付け、原文セクション導線、実行時の注意点をまとめています。

英語版原文:

- [../../sandboxing.md](../../sandboxing.md)

## テーマ位置付け

- 分類: セキュリティと統制
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから、英語版の規範記述に従って実施します。

## 原文セクションガイド

- [H2 · Problem](../../sandboxing.md#problem)
- [H2 · Proposed Solutions](../../sandboxing.md#proposed-solutions)
- [H3 · Option 1: Firejail Integration (Recommended for Linux)](../../sandboxing.md#option-1-firejail-integration-recommended-for-linux)
- [H3 · Option 2: Bubblewrap (Portable, no root required)](../../sandboxing.md#option-2-bubblewrap-portable-no-root-required)
- [H3 · Option 3: Docker-in-Docker (Heavyweight but complete isolation)](../../sandboxing.md#option-3-docker-in-docker-heavyweight-but-complete-isolation)
- [H3 · Option 4: Landlock (Linux Kernel LSM, Rust native)](../../sandboxing.md#option-4-landlock-linux-kernel-lsm-rust-native)
- [H2 · Priority Implementation Order](../../sandboxing.md#priority-implementation-order)
- [H2 · Config Schema Extension](../../sandboxing.md#config-schema-extension)
- [H2 · Testing Strategy](../../sandboxing.md#testing-strategy)

## 実行ヒント

- まず原文の見出し構成を確認し、今回の変更範囲に直結する節から読みます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 仕様解釈に差分が出る場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
