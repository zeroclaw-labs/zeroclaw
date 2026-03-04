# 每夜全功能操作手冊

本操作手冊描述每夜整合矩陣的執行與報告流程。

主要工作流程路徑：`.github/workflows/feature-matrix.yml`，搭配 `profile=nightly`

舊版/僅開發用工作流程範本：`.github/workflows/nightly-all-features.yml`

## 目標

- 持續驗證高風險功能組合（於夜間執行）。
- 產生機器可讀和人類可讀的報告，以利快速排查。

## 通道

- `default`
- `whatsapp-web`
- `browser-native`
- `nightly-all-features`

通道負責人設定於 `.github/release/nightly-owner-routing.json`。

## 產物

- 各通道：`nightly-lane-<lane>` 搭配 `nightly-result-<lane>.json`
- 彙總：`nightly-all-features-summary` 搭配 `nightly-summary.json` 和 `nightly-summary.md`
- 趨勢快照：`nightly-history.json`（最近 3 次完成的 nightly-profile 執行）
- 保留期：通道 + 摘要產物 30 天

## 排程器與啟動注意事項

- 排程合約：每日 `03:15 UTC`（`cron: 15 3 * * *`）。
- 確定性合約：固定 Rust 工具鏈（`1.92.0`）、鎖定 Cargo 命令、all-features 通道明確安裝 apt 套件。
- Nightly profile 執行由 `feature-matrix.yml` 發出；這使手動觸發和排程可從活躍的工作流程目錄中被發現。

## 負責人路由與升級

負責人路由來源：`.github/release/nightly-owner-routing.json`

- `default` -> `@chumyin`
- `whatsapp-web` -> `@chumyin`
- `browser-native` -> `@chumyin`
- `nightly-all-features` -> `@chumyin`

升級閾值：

- 單通道每夜失敗：在排查開始後 30 分鐘內通知對應負責人。
- 同一通道連續 2 次每夜執行失敗：在發行治理討論串中升級並附上兩次執行的 URL。
- 一次每夜執行中 3 個或更多通道失敗：開啟事件議題並呼叫值班維護人員。
- 失敗 24 小時未解決：升級至維護人員列表並封鎖相關發行推進任務。

SLA 目標：

- 確認：工作時段內 30 分鐘內。
- 初步診斷更新：4 小時內。
- 緩解 PR 或回滾決策：24 小時內。

## 可追蹤性（最近 3 次執行）

使用：

- `gh run list --repo zeroclaw-labs/zeroclaw --workflow feature-matrix.yml --limit 10`
- `gh run view <run_id> --repo zeroclaw-labs/zeroclaw --json jobs,headSha,event,createdAt,url`
- 檢查 `nightly-all-features-summary` 產物中的 `nightly-history.json`

手動觸發（nightly profile）：

- `gh workflow run feature-matrix.yml --repo zeroclaw-labs/zeroclaw --ref dev -f profile=nightly -f fail_on_failure=true`

專案更新預期：

- 每週狀態更新附上最近 3 次每夜執行的連結（URL + 結論 + 失敗通道）。

## 失敗處理

1. 檢查 `nightly-summary.md` 找出失敗通道與負責人。
2. 下載失敗通道產物並在本機重新執行確切的命令。
3. 擷取修復 PR + 測試證據。
4. 將修復連結回發行或 CI 治理議題。
5. 若達到升級閾值，在議題更新中加入升級工單/操作手冊動作。
