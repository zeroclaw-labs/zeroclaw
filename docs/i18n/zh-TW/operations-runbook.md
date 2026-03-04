# ZeroClaw 運維手冊（繁體中文）

本手冊適用於負責維護系統可用性、安全態勢及事件回應的維運人員。

最後驗證日期：**2026 年 2 月 18 日**。

## 適用範圍

本文件適用於 Day-2 維運作業：

- 啟動與監督執行環境
- 健康檢查與診斷
- 安全部署與回滾
- 事件分級處理與復原

首次安裝請參閱 [one-click-bootstrap.md](one-click-bootstrap.md)。

## 執行模式

| 模式 | 指令 | 使用時機 |
|---|---|---|
| 前景執行 | `zeroclaw daemon` | 本機除錯、短期工作階段 |
| 僅前景閘道 | `zeroclaw gateway` | webhook 端點測試 |
| 使用者服務 | `zeroclaw service install && zeroclaw service start` | 持久化的維運管理執行環境 |

## 基本維運檢查清單

1. 驗證設定：

```bash
zeroclaw status
```

2. 確認診斷結果：

```bash
zeroclaw doctor
zeroclaw channel doctor
```

3. 啟動執行環境：

```bash
zeroclaw daemon
```

4. 安裝持久化使用者工作階段服務：

```bash
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

## 健康與狀態訊號

| 訊號 | 指令 / 檔案 | 預期結果 |
|---|---|---|
| 設定有效性 | `zeroclaw doctor` | 無嚴重錯誤 |
| 頻道連線狀態 | `zeroclaw channel doctor` | 已設定的頻道均為健康 |
| 執行環境摘要 | `zeroclaw status` | 顯示預期的 provider/model/channels |
| Daemon 心跳/狀態 | `~/.zeroclaw/daemon_state.json` | 檔案定期更新 |

## 日誌與診斷

### macOS / Windows（服務包裝器日誌）

- `~/.zeroclaw/logs/daemon.stdout.log`
- `~/.zeroclaw/logs/daemon.stderr.log`

### Linux（systemd 使用者服務）

```bash
journalctl --user -u zeroclaw.service -f
```

## 事件分級處理流程（快速路徑）

1. 擷取系統狀態快照：

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

2. 檢查服務狀態：

```bash
zeroclaw service status
```

3. 若服務不健康，執行乾淨重啟：

```bash
zeroclaw service stop
zeroclaw service start
```

4. 若頻道仍然故障，驗證 `~/.zeroclaw/config.toml` 中的允許清單與憑證。

5. 若涉及閘道問題，確認繫結/驗證設定（`[gateway]`）與本機可達性。

## 機密洩漏事件回應（CI Gitleaks）

當 `sec-audit.yml` 回報 gitleaks 發現或上傳 SARIF 警示時：

1. 確認該發現是否為真實的憑證洩漏，還是測試/文件的誤報：
   - 審查 `gitleaks.sarif` + `gitleaks-summary.json` 構件
   - 檢查工作流程摘要中的變更 commit 範圍
2. 若為真陽性：
   - 立即撤銷/輪換已暴露的機密
   - 根據政策需要，從可觸及的歷史紀錄中移除洩漏內容
   - 建立事件紀錄並追蹤修復責任歸屬
3. 若為誤報：
   - 優先縮小偵測範圍
   - 僅在附帶明確治理後設資料（`owner`、`reason`、`ticket`、`expires_on`）時才加入允許清單條目
   - 確保相關的治理工單已連結至 PR
4. 重新執行 `Sec Audit` 並確認：
   - gitleaks 階段綠燈
   - 治理防護綠燈
   - SARIF 上傳成功

## 安全變更程序

套用設定變更前：

1. 備份 `~/.zeroclaw/config.toml`
2. 每次只套用一個邏輯性變更
3. 執行 `zeroclaw doctor`
4. 重新啟動 daemon/服務
5. 以 `status` + `channel doctor` 進行驗證

## 回滾程序

若部署導致行為退化：

1. 還原先前的 `config.toml`
2. 重新啟動執行環境（`daemon` 或 `service`）
3. 透過 `doctor` 和頻道健康檢查確認恢復正常
4. 記錄事件根因與緩解措施

## 相關文件

- [one-click-bootstrap.md](one-click-bootstrap.md)
- [troubleshooting.md](troubleshooting.md)
- [config-reference.md](config-reference.md)
- [commands-reference.md](commands-reference.md)
