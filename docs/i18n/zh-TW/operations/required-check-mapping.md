# 必要檢查對應

本文件將合併關鍵工作流程對應至預期的檢查名稱。

## 合併至 `dev` / `main`

| 必要檢查名稱 | 來源工作流程 | 範圍 |
| --- | --- | --- |
| `CI Required Gate` | `.github/workflows/ci-run.yml` | 核心 Rust/文件合併閘門 |
| `Security Audit` | `.github/workflows/sec-audit.yml` | 依賴、密鑰、治理 |
| `Feature Matrix Summary` | `.github/workflows/feature-matrix.yml` | 功能組合編譯矩陣 |
| `Workflow Sanity` | `.github/workflows/workflow-sanity.yml` | 工作流程語法與 lint |

功能矩陣通道檢查名稱（資訊性，非必要）：

- `Matrix Lane (default)`
- `Matrix Lane (whatsapp-web)`
- `Matrix Lane (browser-native)`
- `Matrix Lane (nightly-all-features)`

## 發行 / 預發行

| 必要檢查名稱 | 來源工作流程 | 範圍 |
| --- | --- | --- |
| `Verify Artifact Set` | `.github/workflows/pub-release.yml` | 發行完整性 |
| `Pre-release Guard` | `.github/workflows/pub-prerelease.yml` | 階段推進 + 標籤完整性 |
| `Nightly Summary & Routing` | `.github/workflows/feature-matrix.yml`（`profile=nightly`） | 夜間整合訊號 |

## 驗證程序

1. 取得最新工作流程執行 ID：
   - `gh run list --repo zeroclaw-labs/zeroclaw --workflow feature-matrix.yml --limit 1`
   - `gh run list --repo zeroclaw-labs/zeroclaw --workflow ci-run.yml --limit 1`
2. 列舉檢查/工作名稱並與此對應進行比較：
   - `gh run view <run_id> --repo zeroclaw-labs/zeroclaw --json jobs --jq '.jobs[].name'`
3. 若任何合併關鍵檢查名稱已變更，在修改分支保護策略之前先更新本文件。

## 注意事項

- 所有工作流程 actions 使用固定的 `uses:` 參照。
- 保持檢查名稱穩定；重新命名檢查工作可能會破壞分支保護規則。
- GitHub 對工作流程的排程/手動探索是以預設分支為準。若發行/每夜工作流程僅存在於非預設分支，在預期排程可見性之前需先將其合併至預設分支。
- 每當合併關鍵工作流程/工作被新增或重新命名時更新本對應。
