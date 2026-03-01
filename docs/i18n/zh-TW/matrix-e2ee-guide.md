# Matrix E2EE 指南（繁體中文）

本指南說明如何在 Matrix 房間中穩定執行 ZeroClaw，包括端對端加密（E2EE）房間。

本文聚焦於使用者常見的失敗情境：

> 「Matrix 設定正確、檢查皆通過，但機器人沒有回應。」

## 0. 快速 FAQ（#499 類症狀）

若 Matrix 顯示已連線但沒有回覆，請先驗證以下項目：

1. 發送者在 `allowed_users` 允許清單中（測試用可設為 `["*"]`）。
2. 機器人帳號已加入目標房間。
3. Token 屬於同一個機器人帳號（`whoami` 檢查）。
4. 加密房間有可用的裝置身分（`device_id`）且金鑰已共享。
5. 設定變更後已重新啟動 daemon。

---

## 1. 前置條件

在測試訊息流程前，請確認以下條件全部成立：

1. 機器人帳號已加入目標房間。
2. Access token 屬於同一個機器人帳號。
3. `room_id` 正確：
   - 建議：使用正規房間 ID（`!room:server`）
   - 支援：房間別名（`#alias:server`），ZeroClaw 會自動解析
4. `allowed_users` 允許發送者（開放測試用 `["*"]`）。
5. 在 E2EE 房間中，機器人裝置已收到該房間的加密金鑰。

---

## 2. 設定

使用 `~/.zeroclaw/config.toml`：

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_your_token"

# 可選但建議設定，以提升 E2EE 穩定性：
user_id = "@zeroclaw:matrix.example.com"
device_id = "DEVICEID123"

# 房間 ID 或別名
room_id = "!xtHhdHIIVEZbDPvTvZ:matrix.example.com"
# room_id = "#ops:matrix.example.com"

# 初始驗證時使用 ["*"]，確認功能後再收緊限制。
allowed_users = ["*"]
```

### 關於 `user_id` 和 `device_id`

- ZeroClaw 會嘗試從 Matrix `/_matrix/client/v3/account/whoami` 讀取身分資訊。
- 若 `whoami` 未回傳 `device_id`，請手動設定 `device_id`。
- 這些提示資訊對 E2EE 工作階段恢復特別重要。

---

## 3. 快速驗證流程

1. 執行頻道設定與 daemon：

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

2. 在已設定的 Matrix 房間中發送一則純文字訊息。

3. 確認 ZeroClaw 日誌中包含 Matrix listener 啟動資訊，且沒有重複的同步/驗證錯誤。

4. 在加密房間中，驗證機器人能讀取並回覆來自允許使用者的加密訊息。

---

## 4. 疑難排解「無回應」

請依序使用此檢查清單。

### A. 房間與成員資格

- 確認機器人帳號已加入該房間。
- 若使用別名（`#...`），驗證其解析至預期的正規房間。

### B. 發送者允許清單

- 若 `allowed_users = []`，所有入站訊息皆被拒絕。
- 排查時可暫時設為 `allowed_users = ["*"]`。

### C. Token 與身分

- 使用以下指令驗證 token：

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://matrix.example.com/_matrix/client/v3/account/whoami"
```

- 確認回傳的 `user_id` 與機器人帳號一致。
- 若缺少 `device_id`，請手動設定 `channels_config.matrix.device_id`。

### D. E2EE 相關檢查

- 機器人裝置必須從受信任的裝置接收房間金鑰。
- 若金鑰未共享給此裝置，加密事件將無法解密。
- 請在你的 Matrix 用戶端/管理工作流程中驗證裝置信任與金鑰共享狀態。
- 若日誌顯示 `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found`，表示此裝置尚未啟用金鑰備份恢復。此警告通常不影響即時訊息流程，但仍建議完成金鑰備份/恢復設定。
- 若收件者看到機器人訊息標示為「未驗證」，請從受信任的 Matrix 工作階段驗證/簽署機器人裝置，並確保 `channels_config.matrix.device_id` 在重新啟動之間保持一致。

### E. 訊息格式（Markdown）

- ZeroClaw 以具有 Markdown 能力的 `m.room.message` 文字內容發送 Matrix 文字回覆。
- 支援 `formatted_body` 的 Matrix 用戶端應能正確呈現強調、清單和程式碼區塊。
- 若格式顯示為純文字，請先確認用戶端支援性，再確認 ZeroClaw 正在執行包含 Markdown 啟用 Matrix 輸出的建置版本。

### F. 全新啟動測試

更新設定後，重新啟動 daemon 並發送一則新訊息（不是只查看舊的時間軸歷史）。

---

## 5. 操作注意事項

- 確保 Matrix token 不會出現在日誌和截圖中。
- 先使用寬鬆的 `allowed_users` 設定，確認功能後再收緊為明確的使用者 ID。
- 正式環境建議使用正規房間 ID，避免別名漂移問題。

---

## 6. 相關文件

- [頻道參考](./channels-reference.md)
- [操作日誌關鍵字附錄](./channels-reference.md#7-operations-appendix-log-keywords-matrix)
- [網路部署](./network-deployment.md)
- [通用安全](./agnostic-security.md)
- [審查者手冊](./reviewer-playbook.md)
