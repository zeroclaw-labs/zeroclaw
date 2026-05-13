# はじめに（セットアップガイド）

初回セットアップとクイックオリエンテーションのためのガイドです。

## スタートパス

1. メインの概要とクイックスタート: [../../../README.ja.md](../../../README.ja.md)
2. ワンクリックセットアップとデュアルブートストラップモード: [one-click-bootstrap.md](one-click-bootstrap.md)
3. macOSでのアップデートまたはアンインストール: [macos-update-uninstall.md](macos-update-uninstall.md)
4. タスクからコマンドを探す: [../reference/cli/commands-reference.md](../reference/cli/commands-reference.md)
5. MCPサーバーの登録: [mcp-setup.md](mcp-setup.md)

## パスを選択する

| シナリオ | コマンド |
|----------|---------|
| APIキーを持っていて、最速でセットアップしたい | `daemonclaw onboard --api-key sk-... --provider openrouter` |
| ガイド付きプロンプトを使用したい | `daemonclaw onboard` |
| 設定は存在し、チャンネルの修正だけしたい | `daemonclaw onboard --channels-only` |
| 設定は存在し、意図的にフル上書きしたい | `daemonclaw onboard --force` |
| サブスクリプション認証を使用する | [サブスクリプション認証](../../../README.ja.md#サブスクリプション認証oauth) を参照 |

## オンボーディングと検証

- クイックオンボーディング: `daemonclaw onboard --api-key "sk-..." --provider openrouter`
- ガイド付きオンボーディング: `daemonclaw onboard`
- 既存設定の保護: 再実行には明示的な確認が必要です（非対話型フローでは `--force` が必要）。
- Ollama クラウドモデル (`:cloud`) にはリモートの `api_url` と API キーが必要です (例: `api_url = "https://ollama.com"`)。
- 環境の検証: `daemonclaw status` + `daemonclaw doctor`

## 次のステップ

- ランタイム操作: [../ops/README.md](../ops/README.md)
- リファレンスカタログ: [../reference/README.md](../reference/README.md)
- macOS ライフサイクルタスク: [macos-update-uninstall.md](macos-update-uninstall.md)
