# 金絲雀閘門操作手冊

工作流程：`.github/workflows/ci-canary-gate.yml`
策略：`.github/release/canary-policy.json`

## 輸入參數

- 候選標籤 + 可選 SHA
- 觀測到的錯誤率
- 觀測到的崩潰率
- 觀測到的 p95 延遲
- 觀測到的樣本大小
- `trigger_rollback_on_abort`（僅 workflow_dispatch，預設 `true`）
- `rollback_branch`（僅 workflow_dispatch，預設 `dev`）
- `rollback_target_ref`（可選的明確回滾目標 ref）

## 群組推進

定義於 `.github/release/canary-policy.json`：

- `canary-5pct` 持續 20 分鐘
- `canary-20pct` 持續 20 分鐘
- `canary-50pct` 持續 20 分鐘
- `canary-100pct` 持續 60 分鐘（最終信心視窗）

推進指南：

1. 每個群組視窗先執行 `dry-run`。
2. 只有在決策為 `promote` 時才推進到下一個群組。
3. 在 `hold` 時停止推進並展開調查。
4. 在 `abort` 時觸發回滾流程。

## 決策模型

- `promote`：所有指標皆在設定的閾值範圍內
- `hold`：軟性違規或策略違反（例如樣本不足）
- `abort`：硬性違規（超過閾值 `>1.5x`）

## 可觀測性訊號

受策略保護的訊號：

- `error_rate`
- `crash_rate`
- `p95_latency_ms`
- `sample_size`

所有訊號皆會輸出至 `canary-guard.json` 並呈現在執行摘要中。

## 執行模式

- `dry-run`：僅產生決策 + 產物
- `execute`：允許標記標籤 + 可選的 repository dispatch

## 中止轉回滾整合

當 `decision=abort` 且 `trigger_rollback_on_abort=true` 時，`CI Canary Gate` 會自動觸發 `.github/workflows/ci-rollback.yml`，並帶入受保護的 execute 輸入參數。

觸發的回滾預設值：

- `branch`：工作流程輸入 `rollback_branch`（預設 `dev`）
- `mode`：`execute`
- `allow_non_ancestor`：`false`
- `fail_on_violation`：`true`
- `create_marker_tag`：`true`
- `emit_repository_dispatch`：`true`
- `target_ref`：可選（`rollback_target_ref`），否則回滾守衛使用最新發行標籤策略

金絲雀執行摘要會輸出 `Canary Abort Rollback Trigger` 區段，使觸發行為可供稽核。

## 產物

- `canary-guard.json`
- `canary-guard.md`
- `audit-event-canary-guard.json`

## 操作指南

1. 對每個候選版本先使用 `dry-run`。
2. 絕不在樣本大小低於策略最低值時執行。
3. 對於 `abort`，在發行議題中附上根因摘要並保持候選版本封鎖狀態。
4. 對於啟用自動觸發的 `abort`，驗證關聯的 `CI Rollback Guard` 執行已完成，並檢閱 `ci-rollback-plan` 產物。
