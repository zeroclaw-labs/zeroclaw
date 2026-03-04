# 文件部署策略

本文件定義文件部署的推進與回滾驗證合約。

## 策略來源

- 機器策略：`.github/release/docs-deploy-policy.json`
- 執行腳本：`scripts/ci/docs_deploy_guard.py`
- 工作流程整合：`.github/workflows/docs-deploy.yml`（`docs-quality` 工作）

## 推進合約

對於正式環境部署：

1. 來源分支必須為正式分支（`main`）。
2. 手動正式環境觸發時，當策略要求時必須包含預覽推進證據（`preview_evidence_run_url`）。
3. 守衛輸出必須為 `ready=true` 才能執行 `Deploy Docs to GitHub Pages` 通道。

## 回滾合約

對於手動正式環境回滾：

1. 設定 `deploy_target=production`。
2. 提供 `rollback_ref`（標籤/sha/ref）。
3. 守衛將回滾 ref 解析為 commit SHA。
4. 若策略啟用祖先驗證，回滾 ref 必須是正式分支歷史的祖先。

## 產物與保留

守衛輸出：

- `docs-deploy-guard.json`
- `docs-deploy-guard.md`
- `audit-event-docs-deploy-guard.json`

保留預設值：

- 文件預覽產物：`14` 天
- 文件部署守衛產物：`21` 天

保留值設定於 `.github/release/docs-deploy-policy.json`。
