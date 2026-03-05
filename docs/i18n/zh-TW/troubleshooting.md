# ZeroClaw 故障排除（繁體中文）

本指南聚焦於常見的安裝與執行階段故障，並提供快速解決路徑。

最後驗證日期：**2026 年 2 月 20 日**。

## 安裝 / 引導程式

### `cargo` 找不到

症狀：

- 引導程式因 `cargo is not installed` 而結束

修正方式：

```bash
./bootstrap.sh --install-rust
```

或者從 <https://rustup.rs/> 安裝。

### 缺少系統建置相依套件

症狀：

- 建置因編譯器或 `pkg-config` 問題而失敗

修正方式：

```bash
./bootstrap.sh --install-system-deps
```

### 在低記憶體 / 低磁碟空間主機上建置失敗

症狀：

- `cargo build --release` 被終止（`signal: 9`、OOM killer 或 `cannot allocate memory`）
- 加入 swap 後建置仍然當掉，因為磁碟空間耗盡

原因說明：

- 執行時期記憶體（常見操作低於 5MB）與編譯時期記憶體需求不同。
- 完整原始碼建置可能需要 **2 GB RAM + swap** 以及 **6 GB 以上可用磁碟空間**。
- 在磁碟空間有限的機器上啟用 swap，雖可避免 RAM OOM，但仍可能因磁碟空間不足而失敗。

資源受限主機的建議做法：

```bash
./bootstrap.sh --prefer-prebuilt
```

僅使用預建二進位檔模式（不從原始碼建置）：

```bash
./bootstrap.sh --prebuilt-only
```

如果必須在資源受限主機上從原始碼編譯：

1. 只有在磁碟空間足以同時容納 swap 與建置產出時才加入 swap。
1. 限制 cargo 平行度：

```bash
CARGO_BUILD_JOBS=1 cargo build --release --locked
```

1. 在不需要 Matrix 時減少重量級功能：

```bash
cargo build --release --locked --features hardware
```

1. 在效能較強的機器上交叉編譯，再將二進位檔複製到目標主機。

### 建置非常慢或看似卡住

症狀：

- `cargo check` / `cargo build` 在 `Checking zeroclaw` 停留很久
- 重複出現 `Blocking waiting for file lock on package cache` 或 `build directory`

為何在 ZeroClaw 中會發生：

- Matrix E2EE 堆疊（`matrix-sdk`、`ruma`、`vodozemac`）龐大且型別檢查成本高。
- TLS + 密碼學原生建置腳本（`aws-lc-sys`、`ring`）會增加明顯的編譯時間。
- `rusqlite` 使用內建 SQLite 會在本機編譯 C 程式碼。
- 同時執行多個 cargo 作業/工作樹會造成鎖競爭。

快速檢查：

```bash
cargo check --timings
cargo tree -d
```

時間報告會輸出到 `target/cargo-timings/cargo-timing.html`。

加速本機迭代（不需要 Matrix 頻道時）：

```bash
cargo check
```

這會使用精簡的預設功能集，可顯著減少編譯時間。

明確啟用 Matrix 支援來建置：

```bash
cargo check --features channel-matrix
```

啟用 Matrix + Lark + 硬體支援來建置：

```bash
cargo check --features hardware,channel-matrix,channel-lark
```

鎖競爭排查：

```bash
pgrep -af "cargo (check|build|test)|cargo check|cargo build|cargo test"
```

在執行自己的建置前，先停止不相關的 cargo 作業。

### 安裝後找不到 `zeroclaw` 指令

症狀：

- 安裝成功但 shell 找不到 `zeroclaw`

修正方式：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
which zeroclaw
```

如有需要，請寫入 shell 設定檔以持久化。

## 執行環境 / 閘道

### 閘道無法連線

檢查方式：

```bash
zeroclaw status
zeroclaw doctor
```

確認 `~/.zeroclaw/config.toml` 中的設定：

- `[gateway].host`（預設 `127.0.0.1`）
- `[gateway].port`（預設 `42617`）
- `allow_public_bind` 僅在有意暴露 LAN/公開介面時才啟用

### 配對 / 驗證在 webhook 上失敗

檢查方式：

1. 確認配對流程已完成（`/pair` 流程）
2. 確認 bearer token 為最新
3. 重新執行診斷：

```bash
zeroclaw doctor
```

## 頻道問題

### Telegram 衝突：`terminated by other getUpdates request`

原因：

- 多個輪詢器使用了同一個 bot token

修正方式：

- 確保該 token 只有一個執行中的 runtime
- 停止多餘的 `zeroclaw daemon` / `zeroclaw channel start` 行程

### `channel doctor` 顯示頻道不健康

檢查方式：

```bash
zeroclaw channel doctor
```

接著確認設定中對應頻道的憑證與允許清單欄位。

## 網路存取問題

### `curl`/`wget` 在 shell 工具中被封鎖

症狀：

- 工具輸出包含 `Command blocked: high-risk command is disallowed by policy`
- 模型表示 `curl`/`wget` 被封鎖

原因說明：

- `curl`/`wget` 屬於高風險 shell 指令，可能被自主策略封鎖。

建議做法：

- 使用專用工具取代 shell 的 fetch 操作：
  - `http_request` 用於直接 API/HTTP 呼叫
  - `web_fetch` 用於頁面內容擷取/摘要

最小設定：

```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
provider = "fast_html2md"
allowed_domains = ["*"]
```

### `web_search_tool` 回傳 `403`/`429`

症狀：

- 工具輸出包含 `DuckDuckGo search failed with status: 403`（或 `429`）

原因說明：

- 部分網路/代理/速率限制會封鎖 DuckDuckGo HTML 搜尋端點的流量。

修正選項：

1. 切換至 Brave 搜尋（有 API 金鑰時建議使用）：

```toml
[web_search]
enabled = true
provider = "brave"
brave_api_key = "<SECRET>"
```

2. 切換至 Firecrawl（如果建置中有啟用）：

```toml
[web_search]
enabled = true
provider = "firecrawl"
api_key = "<SECRET>"
```

3. 保留 DuckDuckGo 搜尋，但取得 URL 後改用 `web_fetch` 讀取頁面。

### `web_fetch`/`http_request` 顯示主機不被允許

症狀：

- 出現 `Host '<domain>' is not in http_request.allowed_domains` 之類的錯誤
- 或 `web_fetch tool is enabled but no allowed_domains are configured`

修正方式：

- 加入確切的網域名稱或 `"*"` 以開放公開網路存取：

```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
allowed_domains = ["*"]
blocked_domains = []
```

安全注意事項：

- 即使設定 `"*"`，本機/私有網路目標仍會被封鎖
- 在正式環境中盡量使用明確的網域允許清單

## 服務模式

### 服務已安裝但未執行

檢查方式：

```bash
zeroclaw service status
```

復原方式：

```bash
zeroclaw service stop
zeroclaw service start
```

Linux 日誌：

```bash
journalctl --user -u zeroclaw.service -f
```

## 舊版安裝程式相容性

兩種方式都仍可使用：

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
```

`install.sh` 是相容性入口，會轉發/回退至 bootstrap 行為。

## 還是卡住了？

在提交 issue 時請收集並附上以下輸出：

```bash
zeroclaw --version
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

另請附上作業系統、安裝方式，以及經過脫敏處理的設定片段（不含機密資訊）。

## 相關文件

- [operations-runbook.md](operations-runbook.md)
- [one-click-bootstrap.md](one-click-bootstrap.md)
- [channels-reference.md](channels-reference.md)
- [network-deployment.md](network-deployment.md)
