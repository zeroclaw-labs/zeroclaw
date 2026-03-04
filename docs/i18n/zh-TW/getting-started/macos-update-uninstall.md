# macOS 更新與解除安裝指南

本頁說明 ZeroClaw 在 macOS (OS X) 上支援的更新與解除安裝流程。

最後驗證日期：**2026 年 2 月 22 日**。

## 1) 確認目前的安裝方式

```bash
which zeroclaw
zeroclaw --version
```

常見安裝位置：

- Homebrew：`/opt/homebrew/bin/zeroclaw`（Apple Silicon）或 `/usr/local/bin/zeroclaw`（Intel）
- Cargo/bootstrap/手動安裝：`~/.cargo/bin/zeroclaw`

如果兩者皆存在，Shell 的 `PATH` 順序會決定實際執行的版本。

## 2) 在 macOS 上更新

### A) Homebrew 安裝

```bash
brew update
brew upgrade zeroclaw
zeroclaw --version
```

### B) Clone + bootstrap 安裝

從您的本機 repository checkout 目錄執行：

```bash
git pull --ff-only
./bootstrap.sh --prefer-prebuilt
zeroclaw --version
```

如果您想要僅從原始碼更新：

```bash
git pull --ff-only
cargo install --path . --force --locked
zeroclaw --version
```

### C) 手動預建置二進位檔安裝

使用最新的 release asset 重新執行下載/安裝流程，然後驗證：

```bash
zeroclaw --version
```

## 3) 在 macOS 上解除安裝

### A) 先停止並移除背景服務

此步驟可防止在移除二進位檔後 daemon 繼續執行。

```bash
zeroclaw service stop || true
zeroclaw service uninstall || true
```

`service uninstall` 會移除以下服務產出物：

- `~/Library/LaunchAgents/com.zeroclaw.daemon.plist`

### B) 依安裝方式移除二進位檔

Homebrew：

```bash
brew uninstall zeroclaw
```

Cargo/bootstrap/手動安裝（`~/.cargo/bin/zeroclaw`）：

```bash
cargo uninstall zeroclaw || true
rm -f ~/.cargo/bin/zeroclaw
```

### C) 選用：移除本機執行期資料

僅在您想要完整清除設定檔、驗證設定、日誌和工作區狀態時執行此步驟。

```bash
rm -rf ~/.zeroclaw
```

## 4) 驗證解除安裝是否完成

```bash
command -v zeroclaw || echo "zeroclaw binary not found"
pgrep -fl zeroclaw || echo "No running zeroclaw process"
```

如果 `pgrep` 仍然找到行程，請手動停止並重新檢查：

```bash
pkill -f zeroclaw
```

## 相關文件

- [一鍵安裝引導](../one-click-bootstrap.md)
- [指令參考](../commands-reference.md)
- [疑難排解](../troubleshooting.md)
