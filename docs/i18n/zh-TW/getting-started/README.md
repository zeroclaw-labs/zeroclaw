# 快速入門文件

適用於首次安裝與快速上手。

## 入門路徑

1. 主要概覽與快速啟動：[../../../../README.md](../../../../README.md)
2. 一鍵安裝與雙模式引導：[../one-click-bootstrap.md](../one-click-bootstrap.md)
3. 在 macOS 上更新或移除：[macos-update-uninstall.md](macos-update-uninstall.md)
4. 在 Android 上設定（Termux/ADB）：[../android-setup.md](../android-setup.md)
5. 依任務查詢指令：[../commands-reference.md](../commands-reference.md)

## 選擇您的路徑

| 情境 | 指令 |
|------|------|
| 我有 API 金鑰，想要最快速的設定 | `zeroclaw onboard --api-key sk-... --provider openrouter` |
| 我想要互動式引導 | `zeroclaw onboard --interactive` |
| 設定已存在，只需修正頻道 | `zeroclaw onboard --channels-only` |
| 設定已存在，我刻意要完全覆寫 | `zeroclaw onboard --force` |
| 使用訂閱認證 | 參見 [訂閱認證](../../../../README.md#subscription-auth-openai-codex--claude-code) |

## 初始化與驗證

- 快速初始化：`zeroclaw onboard --api-key "sk-..." --provider openrouter`
- 互動式初始化：`zeroclaw onboard --interactive`
- 既有設定保護：重新執行時需要明確確認（或在非互動式流程中使用 `--force`）
- Ollama 雲端模型（`:cloud`）需要遠端 `api_url` 與 API 金鑰（例如 `api_url = "https://ollama.com"`）。
- 驗證環境：`zeroclaw status` + `zeroclaw doctor`

## 下一步

- 執行時運維：[../operations/README.md](../operations/README.md)
- 參照手冊：[../reference/README.md](../reference/README.md)
- macOS 生命週期任務：[macos-update-uninstall.md](macos-update-uninstall.md)
- Android 設定路徑：[../android-setup.md](../android-setup.md)
