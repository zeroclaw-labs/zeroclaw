# 自架 Runner 修復操作手冊

本操作手冊提供自架 runner 容量事件的操作步驟。

## 範圍

當 CI 工作持續排隊、runner 可用性下降或 runner 主機磁碟空間不足時使用本手冊。

## 腳本

- `scripts/ci/runner_health_report.py`
  - 查詢 GitHub Actions runner 狀態和工作流程佇列壓力。
  - 產生主控台摘要和可選的 JSON 報告。
- `scripts/ci/runner_disk_cleanup.sh`
  - 回收過期的 runner 工作區/暫存/診斷檔案。
  - 預設為 dry-run 模式，需要明確使用 `--apply`。
- `scripts/ci/queue_hygiene.py`
  - 移除過時工作流程和過期重複執行的排隊積壓。
  - 預設為 dry-run 模式；使用 `--apply` 執行取消操作。

## 1) 健康檢查

```bash
python3 scripts/ci/runner_health_report.py \
  --repo zeroclaw-labs/zeroclaw \
  --require-label self-hosted \
  --require-label aws-india \
  --min-online 3 \
  --min-available 1 \
  --max-queued-runs 20 \
  --output-json artifacts/runner-health.json
```

認證注意事項：

- 腳本依序從 `--token`、`GH_TOKEN`/`GITHUB_TOKEN` 讀取 token，最後回退至 `gh auth token`。

建議警報閾值：

- `online < 3`（嚴重）
- `available < 1`（嚴重）
- `queued runs > 20`（嚴重）
- `busy ratio > 90%`（警告）

## 2) 磁碟清理（先 Dry-Run）

```bash
scripts/ci/runner_disk_cleanup.sh \
  --runner-root /home/ubuntu/actions-runner-pool \
  --work-retention-days 2 \
  --diag-retention-days 7
```

套用模式（在排空工作後）：

```bash
scripts/ci/runner_disk_cleanup.sh \
  --runner-root /home/ubuntu/actions-runner-pool \
  --work-retention-days 2 \
  --diag-retention-days 7 \
  --apply
```

可選搭配 Docker 清理：

```bash
scripts/ci/runner_disk_cleanup.sh \
  --runner-root /home/ubuntu/actions-runner-pool \
  --apply \
  --docker-prune
```

安全行為：

- `--apply` 在偵測到 runner worker/listener 行程時會中止，除非提供 `--force`。
- 預設模式為非破壞性。

## 3) 復原流程

1. 若佇列壓力高，暫停或減少非阻擋性工作流程。
2. 執行健康報告並擷取 JSON 產物。
3. 以 dry-run 模式執行磁碟清理，檢閱候選清單。
4. 排空 runner，然後套用清理。
5. 重新執行健康報告並確認佇列/可用性已恢復。

## 4) 佇列衛生（先 Dry-Run）

Dry-run 範例：

```bash
python3 scripts/ci/queue_hygiene.py \
  --repo zeroclaw-labs/zeroclaw \
  --obsolete-workflow "CI Build (Fast)" \
  --dedupe-workflow "CI Run" \
  --output-json artifacts/queue-hygiene.json
```

套用模式：

```bash
python3 scripts/ci/queue_hygiene.py \
  --repo zeroclaw-labs/zeroclaw \
  --obsolete-workflow "CI Build (Fast)" \
  --dedupe-workflow "CI Run" \
  --max-cancel 200 \
  --apply \
  --output-json artifacts/queue-hygiene-applied.json
```

安全行為：

- 至少需要一項策略（`--obsolete-workflow` 或 `--dedupe-workflow`）。
- `--apply` 為選擇性啟用；預設為非破壞性預覽。
- 去重預設僅針對 PR；僅在明確處理 push/手動積壓時使用 `--dedupe-include-non-pr`。
- 取消操作受 `--max-cancel` 限制。

## 注意事項

- 這些腳本為操作工具，不會變更合併閘門策略。
- 閾值應與觀測到的 runner 池大小和流量特徵保持一致。
