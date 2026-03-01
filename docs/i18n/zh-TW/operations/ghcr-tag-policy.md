# GHCR 標籤策略

本文件定義 `.github/workflows/pub-docker-img.yml` 的正式容器標籤合約。

## 策略來源

- 機器策略：`.github/release/ghcr-tag-policy.json`
- 執行腳本：`scripts/ci/ghcr_publish_contract_guard.py`
- 工作流程整合：`.github/workflows/pub-docker-img.yml`（`publish` 工作）
- 相關弱點閘門策略：`.github/release/ghcr-vulnerability-policy.json`（`scripts/ci/ghcr_vulnerability_gate.py`）

## 標籤分類

發行發布僅限於符合 `vX.Y.Z` 的穩定 git 標籤。

每次發布執行時，工作流程必須產生三個 GHCR 標籤：

1. `vX.Y.Z`（發行標籤，不可變）
2. `sha-<12>`（commit SHA 標籤，不可變）
3. `latest`（指向最新穩定版本的可變指標）

## 不可變性合約

守衛執行摘要一致性驗證：

1. `digest(vX.Y.Z) == digest(sha-<12>)`
2. `digest(latest) == digest(vX.Y.Z)`（當 `require_latest_on_release=true` 時）

若任何必要標籤遺失、無法拉取或違反摘要一致性，發布合約驗證即失敗。

## 回滾對應

回滾候選項依策略類別順序（`rollback_priority`）確定性地輸出：

1. `sha-<12>`
2. `vX.Y.Z`

守衛將此對應輸出至 `ghcr-publish-contract.json` 以供稽核。

## 產物與保留

發布執行輸出：

- `ghcr-publish-contract.json`
- `ghcr-publish-contract.md`
- `audit-event-ghcr-publish-contract.json`
- `ghcr-vulnerability-gate.json`
- `ghcr-vulnerability-gate.md`
- `audit-event-ghcr-vulnerability-gate.json`
- Trivy 報告（`trivy-<tag>.sarif`、`trivy-<tag>.txt`、`trivy-<tag>.json`、`trivy-sha-<12>.txt`、`trivy-sha-<12>.json`、`trivy-latest.txt`、`trivy-latest.json`）

保留預設值：

- 合約產物：`21` 天
- 弱點閘門產物：`21` 天
- Trivy 掃描產物：`14` 天

保留值定義於 `.github/release/ghcr-tag-policy.json` 並反映在工作流程產物上傳中。
