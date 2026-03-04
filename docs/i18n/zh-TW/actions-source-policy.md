# Actions 來源政策（第一階段）（繁體中文）

本文件定義此儲存庫目前的 GitHub Actions 來源控制政策。

第一階段目標：以最小干擾鎖定 action 來源，為全面 SHA 固定做準備。

## 目前政策

- Repository Actions 權限：已啟用
- 允許的 actions 模式：選取式
- 是否要求 SHA 固定：否（延後至第二階段）

選取白名單模式：

- `actions/*`（涵蓋 `actions/cache`、`actions/checkout`、`actions/upload-artifact`、`actions/download-artifact` 及其他第一方 actions）
- `docker/*`
- `dtolnay/rust-toolchain@*`
- `DavidAnson/markdownlint-cli2-action@*`
- `lycheeverse/lychee-action@*`
- `EmbarkStudios/cargo-deny-action@*`
- `rustsec/audit-check@*`
- `rhysd/actionlint@*`
- `softprops/action-gh-release@*`
- `sigstore/cosign-installer@*`
- `Checkmarx/vorpal-reviewdog-github-action@*`
- `Swatinem/rust-cache@*`

## 變更控制匯出

使用以下指令匯出目前的有效政策，供稽核/變更控制使用：

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

記錄每次政策變更時需包含：

- 變更日期/時間（UTC）
- 操作者
- 原因
- 白名單差異（新增/移除的模式）
- 回滾說明

## 為何採用此階段

- 降低來自未審查 marketplace actions 的供應鏈風險。
- 以低遷移成本保留現有 CI/CD 功能。
- 為第二階段的全面 SHA 固定做準備，同時不阻擋當前開發。

## 代理工作流程防護

由於此儲存庫具有高比例的代理編寫變更：

- 任何新增或變更 `uses:` action 來源的 PR 都必須包含白名單影響說明。
- 新的第三方 actions 需在加入白名單前取得維護者的明確審查。
- 僅為已驗證缺少的 actions 擴展白名單；避免廣泛的萬用字元例外。
- Actions 政策變更的 PR 描述中須保留回滾指示。

## 驗證檢查清單

白名單變更後需驗證：

1. `CI`
2. `Docker`
3. `Security Audit`
4. `Workflow Sanity`
5. `Release`（在安全時執行）

需注意的失敗模式：

- `action is not allowed by policy`

若遇到此錯誤，僅新增該特定受信任的缺失 action，重新執行，並記錄原因。

最新掃描紀錄：

- 2026-02-21：新增手動 Vorpal reviewdog 工作流程，用於支援檔案類型的針對性安全程式碼檢查
    - 新增白名單模式：`Checkmarx/vorpal-reviewdog-github-action@*`
    - 工作流程使用固定來源：`Checkmarx/vorpal-reviewdog-github-action@8cc292f337a2f1dea581b4f4bd73852e7becb50d`（v1.2.0）
- 2026-02-26：標準化 runner/action 來源用於快取與 Docker 建置路徑
    - 新增白名單模式：`Swatinem/rust-cache@*`
    - Docker 建置作業使用 `docker/setup-buildx-action` 和 `docker/build-push-action`
- 2026-02-16：在 `release.yml` 中發現隱藏相依：`sigstore/cosign-installer@...`
    - 新增白名單模式：`sigstore/cosign-installer@*`
- 2026-02-17：安全稽核可重現性/即時性平衡更新
    - 新增白名單模式：`rustsec/audit-check@*`
    - 以固定的 `rustsec/audit-check@69366f33c96575abad1ee0dba8212993eecbe998` 取代 `security.yml` 中的行內 `cargo install cargo-audit` 執行
    - 取代 #588 中的浮動版本提案，同時保持 action 來源政策明確

## 回滾

緊急解除阻擋路徑：

1. 暫時將 Actions 政策設回 `all`。
2. 找出缺失條目後恢復選取式白名單。
3. 記錄事件與最終白名單差異。
