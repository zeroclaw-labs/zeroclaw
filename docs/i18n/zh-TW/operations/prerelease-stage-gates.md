# 預發行階段閘門

工作流程：`.github/workflows/pub-prerelease.yml`
策略：`.github/release/prerelease-stage-gates.json`

## 階段模型

- `alpha`
- `beta`
- `rc`
- `stable`

## 守衛規則

- 標籤格式：`vX.Y.Z-(alpha|beta|rc).N`
- 階段轉換必須遵循策略（`alpha -> beta -> rc -> stable`）
- 同一語意版本不允許階段降級
- 同階段標籤號必須單調遞增（例如 `alpha.1 -> alpha.2`）
- 標籤 commit 必須可從 `origin/main` 到達
- 標籤處的 `Cargo.toml` 版本必須與標籤版本一致

## 階段閘門矩陣

| 階段 | 必要的前置階段 | 必要檢查 |
| --- | --- | --- |
| `alpha` | - | `CI Required Gate`、`Security Audit` |
| `beta` | `alpha` | `CI Required Gate`、`Security Audit`、`Feature Matrix Summary` |
| `rc` | `beta` | `CI Required Gate`、`Security Audit`、`Feature Matrix Summary`、`Nightly Summary & Routing` |
| `stable` | `rc` | `CI Required Gate`、`Security Audit`、`Feature Matrix Summary`、`Verify Artifact Set`、`Nightly Summary & Routing` |

守衛會驗證策略檔案完整定義此矩陣結構。遺失或格式不正確的矩陣設定會導致驗證失敗。

## 轉換稽核軌跡

`prerelease-guard.json` 現在包含結構化的轉換證據：

- `transition.type`：`initial_stage`、`stage_iteration`、`promotion` 或 `demotion_blocked`
- `transition.outcome`：最終決策（`promotion`、`promotion_blocked`、`demotion_blocked` 等）
- `transition.previous_highest_stage` / `transition.previous_highest_tag`
- `transition.required_previous_stage` / `transition.required_previous_tag`

降級嘗試會被拒絕並記錄為 `demotion_blocked`。

## 發行階段歷史發布

守衛產物發布正在驗證的語意版本的發行階段歷史：

- `stage_history.per_stage`：依 `alpha|beta|rc|stable` 分組的標籤
- `stage_history.timeline`：正規化的階段時間軸項目
- `stage_history.latest_stage` / `stage_history.latest_tag`

相同歷史會呈現在 `prerelease-guard.md` 中並附加至工作流程摘要。

## 輸出

- `prerelease-guard.json`
- `prerelease-guard.md`
- `audit-event-prerelease-guard.json`

## 發布合約

- `dry-run`：僅守衛 + 建置 + 產物清單
- `publish`：建立/更新 GitHub 預發行版本並附加已建置的資產
