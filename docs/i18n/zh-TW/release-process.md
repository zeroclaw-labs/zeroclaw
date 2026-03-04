# ZeroClaw 發佈流程（繁體中文）

本操作手冊定義維護者的標準發佈流程。

最後驗證日期：**2026 年 2 月 25 日**。

## 發佈目標

- 確保發佈流程可預測且可重複。
- 僅從已合併至 `main` 的程式碼進行發佈。
- 在發佈前驗證多目標平台的產出物。
- 即使 PR 量大，也維持穩定的發佈節奏。

## 標準節奏

- 修補版（patch）/次要版（minor）：每週或每兩週一次。
- 緊急安全修補：依需求隨時發佈。
- 絕不等待大量 commit 累積後才一次發佈。

## 工作流程契約

發佈自動化定義於：

- `.github/workflows/pub-release.yml`

模式：

- Tag push `v*`：發佈模式。
- 手動觸發：僅驗證模式或發佈模式。
- 每週排程：僅驗證模式。
- 預發佈 tag（`vX.Y.Z-alpha.N`、`vX.Y.Z-beta.N`、`vX.Y.Z-rc.N`）：預發佈發佈路徑。
- Canary 門檻（每週/手動）：升級/暫停/中止的決策路徑。

發佈模式防護機制：

- Tag 必須符合穩定格式 `vX.Y.Z`（預發佈 tag 由 `Pub Pre-release` 處理）。
- Tag 必須已存在於 origin。
- Tag 必須為標註型（annotated），不接受輕量型（lightweight）tag。
- Tag 對應的 commit 必須可從 `origin/main` 追溯到。
- 觸發發佈的操作者必須在 `RELEASE_AUTHORIZED_ACTORS` 允許清單中。
- 可選的 tagger email 允許清單可透過 `RELEASE_AUTHORIZED_TAGGER_EMAILS` 設定。
- 對應的 GHCR 映像 tag（`ghcr.io/<owner>/<repo>:<tag>`）必須在 GitHub Release 發佈完成前可用。
- 產出物在發佈前完成驗證。
- 觸發來源記錄於 `release-trigger-guard.json` 和 `audit-event-release-trigger-guard.json`。
- 多架構產出物契約由 `.github/release/release-artifact-contract.json` 透過 `release_artifact_guard.py` 強制執行。
- Release notes 包含自動產生的供應鏈證據前言（`release-notes-supply-chain.md`）及 GitHub 產生的 commit 範圍筆記。

## 維護者操作程序

### 1) `main` 分支飛行前檢查

1. 確認最新 `main` 的必要檢查皆為綠燈。
2. 確認目前沒有高優先級事件或已知的回歸問題。
3. 確認安裝器與 Docker 工作流程在近期 `main` commit 上運作正常。

### 2) 執行驗證建置（不發佈）

手動執行 `Pub Release`：

- `publish_release`: `false`
- `release_ref`: `main`

預期結果：

- 完整目標矩陣建置成功。
- `verify-artifacts` 根據 `.github/release/release-artifact-contract.json` 強制檢查封存檔完整性。
- 不會發佈 GitHub Release。
- 產出 `release-trigger-guard` artifact，包含授權/來源證據。
- 產出 `release-artifact-guard-verify` artifact，包含 `release-artifact-guard.verify.json`、`release-artifact-guard.verify.md` 及 `audit-event-release-artifact-guard-verify.json`。

### 3) 建立發佈 tag

從乾淨的本機 checkout（已同步至 `origin/main`）：

```bash
scripts/release/cut_release_tag.sh vX.Y.Z --push
```

此腳本會強制執行：

- 工作目錄無未提交變更
- `HEAD == origin/main`
- Tag 不重複
- 穩定的 semver tag 格式（`vX.Y.Z`）

### 4) 監控發佈執行

Tag push 後，監控：

1. `Pub Release` 發佈模式
2. `Pub Docker Img` 發佈任務

預期發佈產出物：

- 發佈封存檔
- `SHA256SUMS`
- `CycloneDX` 和 `SPDX` SBOM
- cosign 簽章/憑證
- GitHub Release notes + 附件
- `release-artifact-guard.publish.json` + `release-artifact-guard.publish.md`
- `audit-event-release-artifact-guard-publish.json`，證明發佈階段產出物契約完整性
- `zeroclaw.sha256sums.intoto.json` + `audit-event-release-sha256sums-provenance.json`，用於校驗碼來源連結
- `release-notes-supply-chain.md` / `release-notes-supply-chain.json`，包含發佈資產參考（manifest、SBOM、來源證據、guard 稽核產出物）
- Docker 發佈證據來自 `Pub Docker Img`：`ghcr-publish-contract.json` + `audit-event-ghcr-publish-contract.json` + `ghcr-vulnerability-gate.json` + `audit-event-ghcr-vulnerability-gate.json` + Trivy 報告

### 5) 發佈後驗證

1. 確認 GitHub Release 附件可正常下載。
2. 確認 GHCR tag 包含已發佈版本（`vX.Y.Z`）、發佈 commit SHA tag（`sha-<12>`）和 `latest`。
3. 確認 GHCR 摘要一致性證據：
   - `digest(vX.Y.Z) == digest(sha-<12>)`
   - `digest(latest) == digest(vX.Y.Z)`
4. 確認 GHCR 弱點掃描門檻證據（`ghcr-vulnerability-gate.json`）顯示 `ready=true`，且 `audit-event-ghcr-vulnerability-gate.json` 已產出。
5. 驗證依賴發佈資產的安裝路徑（例如 bootstrap 二進位檔下載）。

### 5.1) 廣泛部署前的 Canary 門檻

先以 `dry-run` 模式執行 `CI Canary Gate`（`.github/workflows/ci-canary-gate.yml`），待指標蒐集完畢後再以 `execute` 模式執行。

必要輸入：

- 候選版 tag/SHA
- 觀測到的錯誤率
- 觀測到的崩潰率
- 觀測到的 p95 延遲
- 觀測到的取樣數量

決策輸出：

- `promote`：門檻條件全部通過
- `hold`：證據不足或輕微違反
- `abort`：嚴重違反門檻

中止整合機制：

- 在 `execute` 模式下，若決策為 `abort` 且 `trigger_rollback_on_abort=true`，canary gate 會自動觸發 `CI Rollback Guard`。
- 預設回滾分支為 `dev`（可透過 `rollback_branch` 覆寫）。
- 可選的明確回滾目標可透過 `rollback_target_ref` 傳入。

### 5.2) 預發佈階段推進（alpha/beta/rc/stable 策略）

用於分階段建立發佈信心：

1. 建立並推送階段 tag（`vX.Y.Z-alpha.N`，接著 beta，再來 rc）。
2. `Pub Pre-release` 會驗證：
   - 階段推進順序
   - 階段矩陣完整性（`alpha|beta|rc|stable` 策略涵蓋）
   - 同一階段的單調遞增編號
   - origin/main 祖先關係
   - Cargo 版本/tag 對齊
3. Guard 產出物記錄階段轉換稽核證據與階段歷史：
   - `transition.type` / `transition.outcome`
   - `transition.previous_highest_stage` 和 `transition.required_previous_tag`
   - `stage_history.per_stage` 和 `stage_history.latest_stage`
4. 只有在 guard 通過後才發佈預發佈資產。

## 緊急/復原路徑

若 tag-push 發佈在產出物驗證後失敗：

1. 在 `main` 上修復工作流程或打包問題。
2. 以發佈模式手動重新執行 `Pub Release`，設定：
   - `publish_release=true`
   - `release_tag=<既有 tag>`
   - 在發佈模式下 `release_ref` 會自動鎖定為 `release_tag`
3. 重新驗證已發佈的資產。

若預發佈/canary 流程失敗：

1. 檢查 guard 產出物（`prerelease-guard.json`、`canary-guard.json`）。
2. 對於預發佈失敗，優先檢查 `transition` + `stage_history` 欄位，以分類是升級、階段迭代還是降級被阻擋的嘗試。
3. 修復階段策略或品質回歸問題。
4. 在任何 execute/publish 操作前，先以 `dry-run` 重新執行 guard。

## 操作注意事項

- 保持發佈變更小且可回復。
- 建議每個版本使用一個發佈 issue/檢查清單，讓交接更清晰。
- 避免從臨時的功能分支進行發佈。
