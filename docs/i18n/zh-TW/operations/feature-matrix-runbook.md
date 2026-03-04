# 功能矩陣操作手冊

本操作手冊定義用於驗證關鍵編譯組合的功能矩陣 CI 通道。

工作流程：`.github/workflows/feature-matrix.yml`

設定檔：

- `compile`（預設）：合併閘門編譯組合
- `nightly`：整合導向的每夜通道命令 + 趨勢快照

## 通道

- `default`：`cargo check --locked`
- `whatsapp-web`：`cargo check --locked --no-default-features --features whatsapp-web`
- `browser-native`：`cargo check --locked --no-default-features --features browser-native`
- `nightly-all-features`：`cargo check --locked --all-features`

## 觸發方式

- 在 Rust + 工作流程路徑上對 `dev` / `main` 的 PR 和 push
- 合併佇列（`merge_group`）
- 每週排程（`compile`）
- 每日排程（`nightly`）
- 手動觸發（`profile=compile|nightly`）

## 產物

- 各通道報告（`compile`）：`feature-matrix-<lane>`
- 各通道報告（`nightly`）：`nightly-lane-<lane>`
- 彙總報告：`feature-matrix-summary`（`feature-matrix-summary.json`、`feature-matrix-summary.md`）
- 保留期：通道 + 摘要產物 21 天
- Nightly 設定檔摘要產物：`nightly-all-features-summary`（`nightly-summary.json`、`nightly-summary.md`、`nightly-history.json`）保留 30 天

## 重試策略

- `compile` 設定檔：最大嘗試次數 = 1
- `nightly` 設定檔：最大嘗試次數 = 2（有界單次重試）
- 通道產物記錄 `attempts_used` 和 `max_attempts` 以供稽核

## 必要檢查合約

分支保護應使用穩定、非矩陣展開的檢查名稱作為合併閘門：

- `Feature Matrix Summary`（來自 `feature-matrix.yml`）

矩陣通道工作保持可觀測但不作為必要檢查目標：

- `Matrix Lane (default)`
- `Matrix Lane (whatsapp-web)`
- `Matrix Lane (browser-native)`
- `Matrix Lane (nightly-all-features)`

檢查名稱穩定性規則：

- 不要在未更新 `docs/operations/required-check-mapping.md` 的情況下重新命名上述工作名稱。
- 保持矩陣 include-list 中的通道名稱穩定，以避免檢查名稱偏移。

驗證命令：

- `gh run list --repo zeroclaw-labs/zeroclaw --workflow feature-matrix.yml --limit 3`
- `gh run view <run_id> --repo zeroclaw-labs/zeroclaw --json jobs --jq '.jobs[].name'`

## 失敗排查

1. 開啟 `feature-matrix-summary.md` 識別失敗通道、負責人和失敗命令。
2. 下載通道產物（`nightly-result-<lane>.json`）取得確切命令 + 退出碼。
3. 使用確切命令和工具鏈鎖定（`--locked`）在本機重現。
4. 將本機重現日誌 + 修復 PR 連結附加至活躍的 Linear 執行議題。

## 高頻失敗類型

| 失敗類型 | 訊號 | 初步回應 | 升級觸發條件 |
| --- | --- | --- | --- |
| Rust 依賴鎖定偏移 | `cargo check --locked` 因鎖定不符而失敗 | 僅在需要時執行 `cargo update -p <crate>`；在專門的 PR 中重新產生鎖定檔 | 同一通道連續 2 次執行失敗 |
| Feature flag 編譯偏移（`whatsapp-web`） | 缺少符號或 cfg 限制的模組 | 在本機執行通道命令並檢查 feature 限制的模組引入 | 24 小時內未解決 |
| Feature flag 編譯偏移（`browser-native`） | 平台/feature 綁定編譯錯誤 | 檢查 browser-native cfg 路徑和近期依賴升級 | 24 小時內未解決 |
| 系統套件依賴偏移（`nightly-all-features`） | 缺少 `libudev`/`pkg-config` 或連結器錯誤 | 驗證 apt install 步驟成功；在相同依賴的乾淨容器中重新執行 | 7 天內重複發生 3 次 |
| CI 環境/執行時期回歸 | 通道逾時或基礎設施暫態失敗 | 重新執行一次，與先前成功的執行比較，然後隔離基礎設施與程式碼問題 | 一次執行中影響 2+ 個通道 |
| 摘要彙總合約中斷 | `Feature Matrix Summary` 無法解析產物 | 驗證通道輸出的產物名稱 + JSON schema | 保護分支上的任何合併閘門失敗 |

## 除錯資料預期

- 通道 JSON 必須包含：lane、status、exit_code、duration_seconds、command。
- 摘要 JSON 必須包含：total、passed、failed、各通道列、負責人路由。
- 產物至少保留一個完整的發行週期（目前設定為 21 天）。
