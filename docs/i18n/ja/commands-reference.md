# コマンドリファレンス（日本語）

このページは Wave 1 の初版ローカライズです。ZeroClaw CLI コマンドを素早く参照するための入口です。

英語版原文:

- [../../commands-reference.md](../../commands-reference.md)

## 主な用途

- タスク別に CLI コマンドを確認する
- オプションと動作境界を確認する
- 実行トラブル時に期待挙動を照合する

## 運用ルール

- コマンド名・フラグ名・設定キーは英語のまま保持します。
- 挙動の最終定義は英語版原文を優先します。

## 最新更新

- 英語版原文に `zeroclaw tui` コマンド（`--features tui-ratatui` が必要）が追加され、フルスクリーンのターミナル UI を利用できるようになりました。
- `zeroclaw gateway` は `--new-pairing` をサポートし、既存のペアリングトークンを消去して新しいペアリングコードを生成できます。
- OpenClaw 移行関連の英語原文が更新されました: `zeroclaw onboard --migrate-openclaw`、`zeroclaw migrate openclaw`、およびエージェントツール `openclaw_migration`（ローカライズ追従は継続中）。
