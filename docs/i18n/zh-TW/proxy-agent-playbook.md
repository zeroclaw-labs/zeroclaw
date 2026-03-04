# 代理程式 Proxy 操作手冊（繁體中文）

本手冊提供可直接複製貼上的工具呼叫範例，用於透過 `proxy_config` 設定代理行為。

當你需要快速且安全地切換 proxy 範圍時，請參考本文件。

## 0. 摘要

- **用途：** 提供可直接使用的代理程式工具呼叫，涵蓋 proxy 範圍管理與回退操作。
- **適用對象：** 在需要透過 proxy 連線的網路環境中執行 ZeroClaw 的維運人員與維護者。
- **涵蓋範圍：** `proxy_config` 動作、模式選擇、驗證流程及疑難排解。
- **不涵蓋：** ZeroClaw 執行環境行為以外的通用網路除錯。

---

## 1. 依意圖快速導覽

根據你的需求快速找到對應操作。

### 1.1 僅代理 ZeroClaw 內部流量

1. 使用範圍 `zeroclaw`。
2. 設定 `http_proxy`/`https_proxy` 或 `all_proxy`。
3. 透過 `{"action":"get"}` 驗證。

前往：

- [第 4 節](#4-模式-a--僅代理-zeroclaw-內部流量)

### 1.2 僅代理特定服務

1. 使用範圍 `services`。
2. 在 `services` 中設定具體的服務鍵或萬用字元選擇器。
3. 透過 `{"action":"list_services"}` 驗證覆蓋範圍。

前往：

- [第 5 節](#5-模式-b--僅代理特定服務)

### 1.3 匯出全行程 proxy 環境變數

1. 使用範圍 `environment`。
2. 透過 `{"action":"apply_env"}` 套用。
3. 透過 `{"action":"get"}` 驗證環境快照。

前往：

- [第 6 節](#6-模式-c--全行程環境-proxy)

### 1.4 緊急回退

1. 停用 proxy。
2. 如有需要，清除環境變數匯出。
3. 重新檢查執行環境與環境變數快照。

前往：

- [第 7 節](#7-停用與回退模式)

---

## 2. 範圍決策矩陣

| 範圍 | 影響對象 | 是否匯出環境變數 | 典型用途 |
|---|---|---|---|
| `zeroclaw` | ZeroClaw 內部 HTTP 用戶端 | 否 | 一般執行環境代理，不產生行程層級副作用 |
| `services` | 僅選定的服務鍵/選擇器 | 否 | 針對特定供應商/工具/頻道的精細路由 |
| `environment` | 執行環境 + 行程環境 proxy 變數 | 是 | 需要 `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` 的整合場景 |

---

## 3. 標準安全工作流程

每次 proxy 變更都應遵循以下步驟：

1. 檢查目前狀態。
2. 探索可用的服務鍵/選擇器。
3. 套用目標範圍設定。
4. 驗證執行環境與環境變數快照。
5. 若行為不符預期，執行回退。

工具呼叫：

```json
{"action":"get"}
{"action":"list_services"}
```

---

## 4. 模式 A — 僅代理 ZeroClaw 內部流量

當 ZeroClaw 的供應商/頻道/工具 HTTP 流量需要走 proxy，但不需匯出行程層級的 proxy 環境變數時使用。

工具呼叫：

```json
{"action":"set","enabled":true,"scope":"zeroclaw","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":["localhost","127.0.0.1"]}
{"action":"get"}
```

預期行為：

- 執行環境 proxy 已為 ZeroClaw HTTP 用戶端啟用。
- 不需要匯出 `HTTP_PROXY` / `HTTPS_PROXY` 行程環境變數。

---

## 5. 模式 B — 僅代理特定服務

當僅有系統的部分服務需要走 proxy（例如特定的供應商/工具/頻道）時使用。

### 5.1 指定特定服務

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.http_request","channel.telegram"],"all_proxy":"socks5h://127.0.0.1:1080","no_proxy":["localhost","127.0.0.1",".internal"]}
{"action":"get"}
```

### 5.2 使用選擇器指定

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.*","tool.*"],"http_proxy":"http://127.0.0.1:7890"}
{"action":"get"}
```

預期行為：

- 僅匹配的服務使用 proxy。
- 未匹配的服務直接連線，不走 proxy。

---

## 6. 模式 C — 全行程環境 Proxy

當你明確需要匯出行程環境變數（`HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`）以供執行環境整合使用時。

### 6.1 設定並套用環境範圍

```json
{"action":"set","enabled":true,"scope":"environment","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":"localhost,127.0.0.1,.internal"}
{"action":"apply_env"}
{"action":"get"}
```

預期行為：

- 執行環境 proxy 已啟用。
- 環境變數已匯出至行程。

---

## 7. 停用與回退模式

### 7.1 停用 proxy（預設安全行為）

```json
{"action":"disable"}
{"action":"get"}
```

### 7.2 停用 proxy 並強制清除環境變數

```json
{"action":"disable","clear_env":true}
{"action":"get"}
```

### 7.3 保持 proxy 啟用但僅清除環境變數匯出

```json
{"action":"clear_env"}
{"action":"get"}
```

---

## 8. 常見操作範例

### 8.1 從全環境 proxy 切換至僅服務 proxy

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.http_request"],"all_proxy":"socks5://127.0.0.1:1080"}
{"action":"get"}
```

### 8.2 新增一個受代理的服務

```json
{"action":"set","scope":"services","services":["provider.openai","tool.http_request","channel.slack"]}
{"action":"get"}
```

### 8.3 使用選擇器重設 `services` 清單

```json
{"action":"set","scope":"services","services":["provider.*","channel.telegram"]}
{"action":"get"}
```

---

## 9. 疑難排解

- 錯誤：`proxy.scope='services' requires a non-empty proxy.services list`
  - 修正：設定至少一個具體的服務鍵或選擇器。

- 錯誤：無效的 proxy URL scheme
  - 允許的 scheme：`http`、`https`、`socks5`、`socks5h`。

- Proxy 未如預期生效
  - 執行 `{"action":"list_services"}` 確認服務名稱/選擇器是否正確。
  - 執行 `{"action":"get"}` 檢查 `runtime_proxy` 和 `environment` 快照值。

---

## 10. 相關文件

- [README.md](./README.md) — 文件索引與分類。
- [network-deployment.md](./network-deployment.md) — 端對端網路部署與通道拓撲指引。
- [resource-limits.md](./resource-limits.md) — 網路/工具執行環境的執行安全限制。

---

## 11. 維護備註

- **負責人：** 執行環境與工具維護者。
- **更新時機：** 新增 `proxy_config` 動作、proxy 範圍語意變更或支援的服務選擇器變更時。
- **最後審閱：** 2026-02-18。
