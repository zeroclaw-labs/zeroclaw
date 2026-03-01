# 基於 Enject 啟發的強化筆記

日期：2026-02-28

## 範圍

本文件記錄了針對 `GreatScott/enject` 的聚焦式安全審查，並將有用的控制措施對應到 ZeroClaw 的執行時期/工具。

目標並非與 `enject`（一個專門的機密注入 CLI）達到功能對等，而是引入實用的防護模式以提升代理安全性與營運可靠性。

## 主要 Enject 安全模式

來自 `enject` 架構與原始碼審查：

1. 機密不應以明文形式存在於專案檔案中。
2. 執行時期遇到未解析的機密參考時應以失敗關閉。
3. 機密輸入應避免 shell 歷史記錄與行程引數曝露。
4. 敏感資料在記憶體中應進行歸零處理或最小化生命週期。
5. 加密/寫入應為經驗證的且具原子性。
6. 工具應避免便利功能成為資料外洩管道（例如不提供 `get`/`export`）。

## 應用於 ZeroClaw

### 1) 敏感檔案存取策略已集中管理

實作於：

- `src/security/sensitive_paths.rs`
- `src/tools/file_read.rs`
- `src/tools/file_write.rs`
- `src/tools/file_edit.rs`

新增共用的敏感路徑偵測：

- 完整檔名（`.env`、`.envrc`、`.git-credentials`、金鑰檔名）
- 副檔名（`.pem`、`.key`、`.p12`、`.pfx`、`.ovpn`、`.kubeconfig`、`.netrc`）
- 敏感路徑元件（`.ssh`、`.aws`、`.gnupg`、`.kube`、`.docker`、`.azure`、`.secrets`）

原因：單一分類器可避免工具之間的偏移，並在更多工具被強化時保持執行一致性。

### 2) `file_read` 預設封鎖敏感檔案讀取

實作於 `src/tools/file_read.rs`：

- 在兩個階段強制封鎖：
  - 路徑正規化之前（輸入路徑）
  - 路徑正規化之後（解析後路徑，包含符號連結目標）
- 新增明確的選擇啟用閘門：
  - `autonomy.allow_sensitive_file_reads = true`

原因：這反映了 `enject` 的「明文機密檔案預設為高風險」立場，同時為受控的緊急存取情境保留操作員覆寫權限。

### 3) `file_write` + `file_edit` 預設封鎖敏感檔案寫入/編輯

實作於：

- `src/tools/file_write.rs`
- `src/tools/file_edit.rs`

在兩個階段強制封鎖：

- 路徑正規化之前（輸入路徑）
- 路徑正規化之後（解析後路徑，包含符號連結目標）

新增明確的選擇啟用閘門：

- `autonomy.allow_sensitive_file_writes = true`

原因：與唯讀曝露不同，對承載機密的檔案進行寫入/編輯可能會靜默地損壞憑證、意外輪替值，或在版本控制/工作區狀態中建立外洩產出物。

### 4) 檔案工具的硬連結逃逸防護

實作於：

- `src/security/file_link_guard.rs`
- `src/tools/file_read.rs`
- `src/tools/file_write.rs`
- `src/tools/file_edit.rs`

行為：

- 三個檔案工具均拒絕處理連結計數 > 1 的既有檔案。
- 這封鎖了一類基於路徑的繞過攻擊——工作區檔名被硬連結到外部敏感內容。

原因：正規化與符號連結檢查無法揭示硬連結的來源；連結計數防護是一種保守的失敗關閉保護，對營運的影響很低。

### 5) 組態層級的敏感讀取/寫入閘門

實作於：

- `src/config/schema.rs`
- `src/security/policy.rs`
- `docs/config-reference.md`

新增：

- `autonomy.allow_sensitive_file_reads`（預設：`false`）
- `autonomy.allow_sensitive_file_writes`（預設：`false`）

兩者均對應到執行時期 `SecurityPolicy`。

### 6) Pushover 憑證擷取強化

實作於 `src/tools/pushover.rs`：

- 環境變數優先的憑證來源（`PUSHOVER_TOKEN`、`PUSHOVER_USER_KEY`）
- 保留 `.env` 回退機制以維持相容性
- 當僅設定一個環境變數時產生硬錯誤（部分狀態）
- 當 `.env` 的值為未解析的 `en://` / `ev://` 參考時產生硬錯誤
- 透過 `EnvGuard` + 全域鎖實現測試環境變數隔離

原因：這符合 `enject` 對未解析機密參考的失敗關閉處理方式，並減少意外的明文處理模糊性。

### 7) 非 CLI 核准會話授權現在確實繞過提示

實作於 `src/agent/loop_.rs`：

- `run_tool_call_loop` 現在遵循 `ApprovalManager::is_non_cli_session_granted(tool)`。
- 新增執行時期追蹤事件：`approval_bypass_non_cli_session_grant`。
- 新增回歸測試：
  - `run_tool_call_loop_uses_non_cli_session_grant_without_waiting_for_prompt`

原因：這修復了一個可靠性/安全性缺口——已核准的非 CLI 工具仍可能在等待核准時停滯。

### 8) 出站洩漏防護嚴格模式 + 各傳遞路徑的組態一致性

實作於：

- `src/config/schema.rs`
- `src/channels/mod.rs`
- `src/gateway/mod.rs`
- `src/gateway/ws.rs`
- `src/gateway/openai_compat.rs`

新增出站洩漏策略：

- `security.outbound_leak_guard.enabled`（預設：`true`）
- `security.outbound_leak_guard.action`（`redact` 或 `block`，預設：`redact`）
- `security.outbound_leak_guard.sensitivity`（`0.0..=1.0`，預設：`0.7`）

行為：

- `redact`：保持現有行為，遮蔽偵測到的憑證資料後傳遞回應。
- `block`：當洩漏偵測器匹配時抑制原始回應，並回傳安全的替代文字。
- Gateway 與 WebSocket 現在從執行時期組態讀取此策略，而非使用硬編碼的預設值。
- 與 OpenAI 相容的 `/v1/chat/completions` 路徑現在對非串流與串流回應使用相同的洩漏防護。
- 對於串流模式，當防護啟用時，輸出會先緩衝並消毒後才進行 SSE 發送，以避免原始差量在掃描前被洩漏。

原因：這消除了一個一致性缺口——嚴格的出站控制可以在頻道中被套用，但在 gateway/ws 邊界處被靜默降級。

## 驗證證據

強化後的專項測試與完整程式庫測試均通過：

- `tools::file_write::tests::file_write_blocks_sensitive_file_by_default`
- `tools::file_write::tests::file_write_allows_sensitive_file_when_configured`
- `tools::file_edit::tests::file_edit_blocks_sensitive_file_by_default`
- `tools::file_edit::tests::file_edit_allows_sensitive_file_when_configured`
- `tools::file_read::tests::file_read_blocks_hardlink_escape`
- `tools::file_write::tests::file_write_blocks_hardlink_target_file`
- `tools::file_edit::tests::file_edit_blocks_hardlink_target_file`
- `channels::tests::process_channel_message_executes_tool_calls_instead_of_sending_raw_json`
- `channels::tests::process_channel_message_telegram_does_not_persist_tool_summary_prefix`
- `channels::tests::process_channel_message_streaming_hides_internal_progress_by_default`
- `channels::tests::process_channel_message_streaming_shows_internal_progress_on_explicit_request`
- `channels::tests::process_channel_message_executes_tool_calls_with_alias_tags`
- `channels::tests::process_channel_message_respects_configured_max_tool_iterations_above_default`
- `channels::tests::process_channel_message_reports_configured_max_tool_iterations_limit`
- `agent::loop_::tests::run_tool_call_loop_uses_non_cli_session_grant_without_waiting_for_prompt`
- `channels::tests::sanitize_channel_response_blocks_detected_credentials_when_configured`
- `gateway::mod::tests::sanitize_gateway_response_blocks_detected_credentials_when_configured`
- `gateway::ws::tests::sanitize_ws_response_blocks_detected_credentials_when_configured`
- `cargo test -q --lib` => 通過（`3760 passed; 0 failed; 4 ignored`）

## 殘餘風險與後續強化步驟

1. 如果模型被誘導列印工具輸出中的機密，執行時期資料外洩仍有可能。
2. 子行程環境中的機密對於具有同等主機權限的行程仍可讀取。
3. `file_read` 以外的某些工具路徑可能仍接受高敏感度資料而未進行統一策略檢查。

建議的後續工作：

1. 集中化一個共用的 `SensitiveInputPolicy`，供所有涉及機密的工具使用（不僅限於 `file_read`）。
2. 為工具憑證流程引入型別化的機密包裝器，以減少 `String` 的生命週期和意外記錄。
3. 將洩漏防護策略一致性檢查擴展到 channel/gateway/ws 以外的任何未來出站介面。
4. 新增涵蓋所有消費憑證工具的「未解析機密參考」行為端對端測試。
