# CI 工作流程對照表（繁體中文）

本文件說明每個 GitHub 工作流程的功能、執行時機，以及是否應阻擋合併。

關於 PR、合併、推送及發布的逐事件交付行為，請參閱 [`.github/workflows/main-branch-flow.md`](../../../.github/workflows/main-branch-flow.md)。

## 阻擋合併 vs 選用

阻擋合併的檢查應保持小巧且確定性。選用檢查適用於自動化與維護，但不應阻擋正常開發。

### 阻擋合併

- `.github/workflows/ci-run.yml`（`CI`）
    - 用途：Rust 驗證（`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets -- -D clippy::correctness`、變更 Rust 行的嚴格差異 lint 閘門、`test`、release 建置煙霧測試）+ 文件變更時的文件品質檢查（`markdownlint` 僅阻擋變更行的問題；連結檢查僅掃描變更行新增的連結）
    - 附加行為：對於涉及 Rust 的 PR 和推送，`CI Required Gate` 要求 `lint` + `test` + `build`（無 PR 專屬建置捷徑）
    - 附加行為：`lint`、`test` 和 `build` 平行執行（皆僅依賴 `changes` 作業）以最小化關鍵路徑耗時
    - 附加行為：rust-cache 透過統一的 `prefix-key`（`ci-run-check`）在 `lint` 和 `test` 之間共享，減少重複編譯；`build` 使用獨立的 key 對應 release-fast 設定檔
    - 附加行為：不穩定測試偵測整合於 `test` 作業中，透過單次重試探測；在懷疑不穩定時產出 `test-flake-probe` 產物；可透過儲存庫變數 `CI_BLOCK_ON_FLAKE_SUSPECTED=true` 啟用選用阻擋
    - 附加行為：變更 CI/CD 管轄路徑的 PR 需取得 `@chumyin` 的明確核准審查（`.github/workflows/**`、`.github/codeql/**`、`.github/connectivity/**`、`.github/release/**`、`.github/security/**`、`.github/actionlint.yaml`、`.github/dependabot.yml`、`scripts/ci/**`，以及 CI 治理文件）
    - 附加行為：變更根目錄授權檔案（`LICENSE-APACHE`、`LICENSE-MIT`）的 PR 必須由 `willsarg` 提交
    - 附加行為：當 PR 的 lint/文件閘門失敗時，CI 發布含失敗閘門名稱與本機修復指令的可操作回饋留言
    - 合併閘門：`CI Required Gate`
- `.github/workflows/workflow-sanity.yml`（`Workflow Sanity`）
    - 用途：GitHub 工作流程檔案的 lint（`actionlint`、tab 檢查）
    - 建議用於變更工作流程的 PR
- `.github/workflows/pr-intake-checks.yml`（`PR Intake Checks`）
    - 用途：安全的 CI 前置 PR 檢查（範本完整性、新增行的 tab/尾隨空白/衝突標記）並附即時固定回饋留言

### 非阻擋但重要

- `.github/workflows/pub-docker-img.yml`（`Docker`）
    - 用途：`dev`/`main` PR 的 Docker 煙霧測試，以及標籤推送（`v*`）時的映像檔發布
    - 附加行為：`ghcr_publish_contract_guard.py` 依據 `.github/release/ghcr-tag-policy.json` 強制執行 GHCR 發布契約（`vX.Y.Z`、`sha-<12>`、`latest` 摘要一致性 + 回滾映射證據）
    - 附加行為：`ghcr_vulnerability_gate.py` 依據 `.github/release/ghcr-vulnerability-policy.json` 強制執行政策驅動的 Trivy 閘門 + 一致性檢查，並產出 `ghcr-vulnerability-gate` 稽核證據
- `.github/workflows/feature-matrix.yml`（`Feature Matrix`）
    - 用途：`default`、`whatsapp-web`、`browser-native` 及 `nightly-all-features` 通道的編譯期矩陣驗證
    - 附加行為：推送觸發的矩陣執行限於 `dev` 分支的 Rust/工作流程路徑變更，以避免在 `main` 上的重複後合併展開
    - 附加行為：PR 上僅在套用 `ci:full` 或 `ci:feature-matrix` 標籤時才執行通道（推送至 dev 和排程則無條件執行）
    - 附加行為：每個通道產出機器可讀的結果產物；摘要通道依據 `.github/release/nightly-owner-routing.json` 彙總負責人路由
    - 附加行為：支援 `compile`（合併閘門）和 `nightly`（整合導向）設定檔，含限制重試政策和趨勢快照產物（`nightly-history.json`）
    - 附加行為：必要檢查映射錨定於穩定的作業名稱 `Feature Matrix Summary`；通道作業保持為資訊性質
- `.github/workflows/nightly-all-features.yml`（`Nightly All-Features`）
    - 用途：舊版/dev 專用的 nightly 範本；主要 nightly 訊號由 `feature-matrix.yml` nightly 設定檔產出
    - 附加行為：負責人路由 + 升級政策記載於 `docs/operations/nightly-all-features-runbook.md`
- `.github/workflows/sec-audit.yml`（`Security Audit`）
    - 用途：相依套件安全通報（`rustsec/audit-check`，固定 SHA）、政策/授權檢查（`cargo deny`）、gitleaks 基礎的機密治理（白名單政策中繼資料 + 過期防護），以及 SBOM 快照產物（`CycloneDX` + `SPDX`）
- `.github/workflows/sec-codeql.yml`（`CodeQL Analysis`）
    - 用途：PR/推送（Rust/codeql 路徑）的靜態安全分析，加上排程/手動執行
- `.github/workflows/ci-change-audit.yml`（`CI/CD Change Audit`）
    - 用途：CI/安全工作流程變更的機器可稽核差異報告（行數變動、新 `uses:` 參考、未固定 action 政策違規、管線至 shell 政策違規、廣泛 `permissions: write-all` 授權、新 `pull_request_target` 觸發引入、新機密參考）
- `.github/workflows/ci-provider-connectivity.yml`（`CI Provider Connectivity`）
    - 用途：排程/手動/提供者清單探測矩陣，含可下載的 JSON/Markdown 產物，用於提供者端點可達性
- `.github/workflows/ci-reproducible-build.yml`（`CI Reproducible Build`）
    - 用途：確定性建置偏移探測（雙次乾淨建置雜湊比對）含結構化產物
- `.github/workflows/ci-supply-chain-provenance.yml`（`CI Supply Chain Provenance`）
    - 用途：release-fast 產物溯源聲明生成 + 無金鑰簽章套件，用於供應鏈可追溯性
- `.github/workflows/ci-rollback.yml`（`CI Rollback Guard`）
    - 用途：確定性回滾計畫生成，含防護式執行模式、標記標籤選項、回滾稽核產物，以及用於 canary 中止自動觸發的分發契約
- `.github/workflows/sec-vorpal-reviewdog.yml`（`Sec Vorpal Reviewdog`）
    - 用途：手動安全程式碼回饋掃描，支援非 Rust 檔案（`.py`、`.js`、`.jsx`、`.ts`、`.tsx`），使用 reviewdog 標註
    - 雜訊控制：預設排除常見測試/fixture 路徑與測試檔案模式（`include_tests=false`）
- `.github/workflows/pub-release.yml`（`Release`）
    - 用途：在驗證模式（手動/排程）下建置發布產物，並在標籤推送或手動發布模式下發布 GitHub releases
- `.github/workflows/pr-label-policy-check.yml`（`Label Policy Sanity`）
    - 用途：驗證 `.github/label-policy.json` 中的共用貢獻者層級政策，並確保標籤工作流程消費該政策

### 選用的儲存庫自動化

- `.github/workflows/pr-labeler.yml`（`PR Labeler`）
    - 用途：範圍/路徑標籤 + 大小/風險標籤 + 細粒度模組標籤（`<module>: <component>`）
    - 附加行為：標籤描述作為懸停提示自動管理，解釋每項自動判斷規則
    - 附加行為：提供者相關關鍵字在提供者/設定/引導/整合變更中被提升為 `provider:*` 標籤（例如 `provider:kimi`、`provider:deepseek`）
    - 附加行為：階層式去重僅保留最精確的範圍標籤（例如 `tool:composio` 抑制 `tool:core` 和 `tool`）
    - 附加行為：模組命名空間壓縮——單一具體模組保留 `prefix:component`；多個具體模組收合至僅 `prefix`
    - 附加行為：依合併 PR 數量在 PR 上套用貢獻者層級（`trusted` >=5、`experienced` >=10、`principal` >=20、`distinguished` >=50）
    - 附加行為：最終標籤集依優先順序排列（`risk:*` 優先，其次 `size:*`，再次貢獻者層級，然後模組/路徑標籤）
    - 附加行為：受管標籤顏色依顯示順序排列，在多標籤時呈現平滑的左至右漸層
    - 手動治理：支援 `workflow_dispatch` 的 `mode=audit|repair`，用於檢視/修復全儲存庫的受管標籤中繼資料偏差
    - 附加行為：風險 + 大小標籤在 PR 生命週期事件（`opened`/`reopened`/`synchronize`/`ready_for_review`）時重新計算；維護者可使用手動 `workflow_dispatch`（`mode=repair`）在例外手動編輯後重新同步受管標籤中繼資料
    - 高風險啟發式路徑：`src/security/**`、`src/runtime/**`、`src/gateway/**`、`src/tools/**`、`.github/workflows/**`
    - 防護：維護者可套用 `risk: manual` 凍結自動風險重算
- `.github/workflows/pr-auto-response.yml`（`PR Auto Responder`）
    - 用途：首次貢獻者引導 + 標籤驅動的回應路由（`r:support`、`r:needs-repro` 等）
    - 附加行為：依合併 PR 數量在 Issue 上套用貢獻者層級（`trusted` >=5、`experienced` >=10、`principal` >=20、`distinguished` >=50），完全匹配 PR 層級閾值
    - 附加行為：貢獻者層級標籤視為自動化管理（PR/Issue 上的手動新增/移除會被自動修正）
    - 防護：標籤驅動的關閉路由僅限 Issue；PR 永遠不會被路由標籤自動關閉
- `.github/workflows/pr-check-stale.yml`（`Stale`）
    - 用途：過期 Issue/PR 生命週期自動化
- `.github/dependabot.yml`（`Dependabot`）
    - 用途：分群、限速的相依套件更新 PR（Cargo + GitHub Actions）
- `.github/workflows/pr-check-status.yml`（`PR Hygiene`）
    - 用途：提醒過期但仍活躍的 PR 進行 rebase/重新執行必要檢查，避免佇列飢餓

## 觸發對照表

- `CI`：推送至 `dev` 和 `main`、向 `dev` 和 `main` 開的 PR、`dev`/`main` 的合併佇列 `merge_group`
- `Docker`：標籤推送（`v*`）用於發布、匹配的 `dev`/`main` PR 用於煙霧建置、手動分發僅用於煙霧測試
- `Feature Matrix`：`dev` 上 Rust + 工作流程路徑推送、合併佇列、每週排程、手動分發；PR 僅在套用 `ci:full` 或 `ci:feature-matrix` 標籤時執行
- `Nightly All-Features`：每日排程與手動分發
- `Release`：標籤推送（`v*`）、每週排程（僅驗證）、手動分發（驗證或發布）
- `Security Audit`：推送至 `dev` 和 `main`、向 `dev` 和 `main` 開的 PR、每週排程
- `Sec Vorpal Reviewdog`：僅手動分發
- `Workflow Sanity`：當 `.github/workflows/**`、`.github/*.yml` 或 `.github/*.yaml` 變更時的 PR/推送
- `Dependabot`：所有更新 PR 目標為 `main`（非 `dev`）
- `PR Intake Checks`：`pull_request_target` 於 opened/reopened/synchronize/ready_for_review
- `Label Policy Sanity`：當 `.github/label-policy.json`、`.github/workflows/pr-labeler.yml` 或 `.github/workflows/pr-auto-response.yml` 變更時的 PR/推送
- `PR Labeler`：`pull_request_target` 於 opened/reopened/synchronize/ready_for_review
- `PR Auto Responder`：issue opened/labeled、`pull_request_target` opened/labeled
- `Test E2E`：推送至 `dev`/`main` 的 Rust 影響路徑（`Cargo*`、`src/**`、`crates/**`、`tests/**`、`scripts/**`）及手動分發
- `Stale PR Check`：每日排程、手動分發
- `PR Hygiene`：每 12 小時排程、手動分發

## 快速分類指南

1. `CI Required Gate` 失敗：從 `.github/workflows/ci-run.yml` 開始檢查。
2. PR 上的 Docker 失敗：檢查 `.github/workflows/pub-docker-img.yml` 的 `pr-smoke` 作業。
   - 標籤發布失敗時，檢查 `ghcr-publish-contract.json` / `audit-event-ghcr-publish-contract.json`、`ghcr-vulnerability-gate.json` / `audit-event-ghcr-vulnerability-gate.json`，以及 `pub-docker-img.yml` 的 Trivy 產物。
3. Release 失敗（標籤/手動/排程）：檢查 `.github/workflows/pub-release.yml` 與 `prepare` 作業輸出。
4. 安全失敗：檢查 `.github/workflows/sec-audit.yml` 與 `deny.toml`。
5. 工作流程語法/lint 失敗：檢查 `.github/workflows/workflow-sanity.yml`。
6. PR 進件失敗：檢查 `.github/workflows/pr-intake-checks.yml` 的固定留言與執行日誌。
7. 標籤政策一致性失敗：檢查 `.github/workflows/pr-label-policy-check.yml`。
8. CI 中的文件失敗：檢查 `.github/workflows/ci-run.yml` 中 `docs-quality` 作業日誌。
9. CI 中的嚴格差異 lint 失敗：檢查 `lint-strict-delta` 作業日誌，並比對 `BASE_SHA` diff 範圍。

## 維護規則

- 保持阻擋合併的檢查具確定性且可重現（適用時使用 `--locked`）。
- 在必要工作流程（`ci-run`、`sec-audit` 和 `sec-codeql`）上支援 `merge_group`，以保持合併佇列相容性。
- 將 PR 映射至 Linear issue key（`RMN-*`/`CDV-*`/`COM-*`），透過 PR 進件檢查。
- 保持 `deny.toml` 安全通報忽略條目為物件格式並附明確理由（由 `deny_policy_guard.py` 強制執行）。
- 保持 deny 忽略治理中繼資料在 `.github/security/deny-ignore-governance.json` 中為最新狀態（由 `deny_policy_guard.py` 強制執行 owner/reason/expiry/ticket）。
- 保持 gitleaks 白名單治理中繼資料在 `.github/security/gitleaks-allowlist-governance.json` 中為最新狀態（由 `secrets_governance_guard.py` 強制執行 owner/reason/expiry/ticket）。
- 保持稽核事件 schema + 保留中繼資料與 `docs/audit-event-schema.md` 一致（`emit_audit_event.py` 信封 + 工作流程產物政策）。
- 保持回滾操作受防護且可逆（`ci-rollback.yml` 預設為 `dry-run`；`execute` 為手動且受政策閘門限制）。
- 保持 canary 政策閾值與抽樣規則在 `.github/release/canary-policy.json` 中為最新狀態。
- 保持 GHCR 標籤分類法與不可變性政策在 `.github/release/ghcr-tag-policy.json` 和 `docs/operations/ghcr-tag-policy.md` 中為最新狀態。
- 保持 GHCR 弱點閘門政策在 `.github/release/ghcr-vulnerability-policy.json` 和 `docs/operations/ghcr-vulnerability-policy.md` 中為最新狀態。
- 保持預發布階段轉換政策 + 矩陣覆蓋 + 轉換稽核語義在 `.github/release/prerelease-stage-gates.json` 中為最新狀態。
- 在變更分支保護設定前，保持必要檢查命名穩定且記載於 `docs/operations/required-check-mapping.md`。
- 遵循 `docs/release-process.md` 的先驗證後發布的發布節奏與標籤紀律。
- 保持阻擋合併的 Rust 品質政策在 `.github/workflows/ci-run.yml`、`dev/ci.sh` 和 `.githooks/pre-push` 之間一致（`./scripts/ci/rust_quality_gate.sh` + `./scripts/ci/rust_strict_delta_gate.sh`）。
- 使用 `./scripts/ci/rust_strict_delta_gate.sh`（或 `./dev/ci.sh lint-delta`）作為變更 Rust 行的增量嚴格合併閘門。
- 定期透過 `./scripts/ci/rust_quality_gate.sh --strict`（例如透過 `./dev/ci.sh lint-strict`）執行完整嚴格 lint 稽核，並在聚焦 PR 中追蹤清理。
- 透過 `./scripts/ci/docs_quality_gate.sh` 保持文件 markdown 閘門為增量式（阻擋變更行問題，另行回報基線問題）。
- 透過 `./scripts/ci/collect_changed_links.py` + lychee 保持文件連結閘門為增量式（僅檢查變更行新增的連結）。
- 保持文件部署政策在 `.github/release/docs-deploy-policy.json`、`docs/operations/docs-deploy-policy.md` 和 `docs/operations/docs-deploy-runbook.md` 中為最新狀態。
- 偏好明確的工作流程權限（最小權限原則）。
- 保持 Actions 來源政策限制於核准的白名單模式（參閱 `docs/actions-source-policy.md`）。
- 在可行時對高成本工作流程使用路徑過濾器。
- 保持文件品質檢查低雜訊（增量 markdown + 增量新增連結檢查）。
- 使用 `scripts/ci/queue_hygiene.py` 在 runner 壓力事件期間有控制地清理過時或被取代的佇列執行。
- 控制相依套件更新量（分群 + PR 上限）。
- 透過儲存庫管理的固定安裝器搭配校驗和驗證安裝第三方 CI 工具（例如 `scripts/ci/install_gitleaks.sh`、`scripts/ci/install_syft.sh`）；避免遠端 `curl | sh` 模式。
- 避免將引導/社群自動化與合併閘門邏輯混合。

## 自動化副作用控制

- 偏好確定性自動化，可在上下文細膩時手動覆寫（`risk: manual`）。
- 保持自動回應留言去重以避免分類雜訊。
- 保持自動關閉行為限於 Issue；維護者擁有 PR 的關閉/合併決定權。
- 若自動化錯誤，先修正標籤，再附明確理由繼續審查。
- 使用 `superseded` / `stale-candidate` 標籤在深度審查前清理重複或休眠的 PR。
