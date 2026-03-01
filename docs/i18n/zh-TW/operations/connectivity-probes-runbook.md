# 連線探測操作手冊

本操作手冊定義維護人員如何在 CI 中操作提供者端點連線探測。

最後驗證日期：**2026 年 2 月 24 日**。

## 範圍

主要工作流程：

- `.github/workflows/ci-provider-connectivity.yml`

探測引擎與設定：

- `scripts/ci/provider_connectivity_matrix.py`
- `.github/connectivity/providers.json`

## 探測模型

設定檔：`.github/connectivity/providers.json`

每個提供者項目定義：

- `id`：提供者識別碼
- `url`：要探測的端點 URL
- `method`：HTTP 方法（`HEAD` 或 `GET`）
- `critical`：失敗時是否應在強制模式下作為閘門

全域欄位：

- `global_timeout_seconds`：DNS + HTTP 檢查的探測逾時時間

## 觸發與強制執行

`CI Provider Connectivity` 行為：

- 排程：每 6 小時
- 手動觸發：`fail_on_critical=true|false`
- PR/push：在探測設定/腳本/工作流程變更時執行

強制執行策略：

- 關鍵端點不可達 + `fail_on_critical=true` -> 工作流程失敗
- 非關鍵端點不可達 -> 回報但不阻擋

## CI 產物

每次執行的產物包含：

- `provider-connectivity-matrix.json`
- `provider-connectivity-matrix.md`
- 工作流程輸出的正規化稽核事件 JSON

Markdown 摘要會附加至 `GITHUB_STEP_SUMMARY`。

## 本機重現

強制模式：

```bash
python3 scripts/ci/provider_connectivity_matrix.py \
  --config .github/connectivity/providers.json \
  --output-json provider-connectivity-matrix.json \
  --output-md provider-connectivity-matrix.md \
  --fail-on-critical
```

僅報告模式：

```bash
python3 scripts/ci/provider_connectivity_matrix.py \
  --config .github/connectivity/providers.json \
  --output-json provider-connectivity-matrix.json \
  --output-md provider-connectivity-matrix.md
```

## 問題排查流程

1. 閱讀矩陣 Markdown 快速查看狀態。
2. 對於失敗項目，檢查 JSON 中各列的欄位：
   - `dns_ok`
   - `http_status`
   - `reachable`
   - `notes`
3. 依類別解決：
   - DNS/傳輸錯誤：檢查網路、提供者狀態，手動重試
   - HTTP 401/403：輪換憑證或驗證認證設定
   - HTTP 404/5xx：驗證端點合約與上游服務健康狀態
4. 在升級持續性事件之前先手動重新執行。

## 變更控管

編輯 `.github/connectivity/providers.json` 時：

1. 保持關鍵端點清單精簡且穩定。
2. 記錄端點關鍵程度變更的原因。
3. 合併前先在本機執行一次探測。
4. 當合約欄位或閘門行為變更時更新本操作手冊。
