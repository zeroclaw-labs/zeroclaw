# 文件部署操作手冊

工作流程：`.github/workflows/docs-deploy.yml`

## 策略合約

- 策略檔案：`.github/release/docs-deploy-policy.json`
- 守衛腳本：`scripts/ci/docs_deploy_guard.py`
- 守衛產物：
  - `docs-deploy-guard.json`
  - `docs-deploy-guard.md`
  - `audit-event-docs-deploy-guard.json`

## 通道

- `Docs Quality Gate`：Markdown 品質 + 新增連結檢查
- `Docs Preview Artifact`：PR/手動預覽套件
- `Deploy Docs to GitHub Pages`：正式環境部署通道

## 觸發方式

- 文件或 README Markdown 變更時的 PR/push
- 手動觸發用於預覽或正式環境
- 手動正式環境支援透過 `rollback_ref` 進行可選回滾

## 品質控管

- `scripts/ci/docs_quality_gate.sh`
- `scripts/ci/collect_changed_links.py` + lychee 新增連結檢查

## 部署規則

- 預覽：僅上傳 `docs-preview` 產物
- 正式環境：在 `main` push 或從 `main` 手動正式環境觸發時部署至 GitHub Pages
- 手動正式環境推進在策略要求時需要 `preview_evidence_run_url`
- `rollback_ref`（僅限手動正式環境）在策略要求祖先驗證時，必須解析為正式分支（`main`）的祖先 commit

## 失敗處理

1. 在本機重新執行 Markdown 與連結閘門。
2. 優先修復壞掉的連結 / Markdown 回歸問題。
3. 僅在預覽產物檢查通過後重新觸發正式環境部署。
4. 檢查 `docs-deploy-guard.json` / `audit-event-docs-deploy-guard.json` 以了解推進/回滾合約違規。

## 回滾驗證（手動演練）

使用 `workflow_dispatch` 並設定：

- `deploy_target=production`
- `preview_evidence_run_url=<成功預覽執行的連結>`
- `rollback_ref=<已知良好的 commit/標籤>`

驗證預期：

1. 守衛模式解析為 `rollback`。
2. 守衛 `ready=true` 且無違規。
3. 部署摘要顯示來源 ref 等於回滾 commit SHA。
4. Pages 部署成功完成。
