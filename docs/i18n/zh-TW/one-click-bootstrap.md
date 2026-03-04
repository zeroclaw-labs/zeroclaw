# 一鍵快速安裝（繁體中文）

本頁說明安裝及初始化 ZeroClaw 的最快途徑。

最後驗證日期：**2026 年 2 月 20 日**。

## 方式 0：Homebrew（macOS/Linuxbrew）

```bash
brew install zeroclaw
```

## 方式 A（建議）：Clone + 本機腳本

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

預設執行內容：

1. `cargo build --release --locked`
2. `cargo install --path . --force --locked`

### 資源預檢與預建置流程

從原始碼建置通常至少需要：

- **2 GB 記憶體 + swap**
- **6 GB 可用磁碟空間**

當系統資源受限時，bootstrap 會先嘗試下載預建置二進位檔案。

```bash
./bootstrap.sh --prefer-prebuilt
```

若要強制僅使用二進位安裝，當找不到相容的 release asset 時直接失敗：

```bash
./bootstrap.sh --prebuilt-only
```

若要跳過預建置流程，強制從原始碼編譯：

```bash
./bootstrap.sh --force-source-build
```

## 雙模式 bootstrap

預設行為為 **app-only**（僅建置/安裝 ZeroClaw），並假定系統已有 Rust 工具鏈。

若是全新的機器，需明確啟用環境 bootstrap：

```bash
./bootstrap.sh --install-system-deps --install-rust
```

說明：

- `--install-system-deps` 安裝編譯器及建置相關套件（可能需要 `sudo`）。
- `--install-rust` 在 Rust 尚未安裝時透過 `rustup` 進行安裝。
- `--prefer-prebuilt` 先嘗試下載 release 二進位檔，失敗時才回退到原始碼建置。
- `--prebuilt-only` 停用原始碼回退機制。
- `--force-source-build` 完全停用預建置流程。

## 方式 B：遠端一行指令

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash
```

對於高安全性環境，建議使用方式 A，以便在執行前先檢視腳本內容。

舊版相容指令：

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
```

此舊版端點會優先轉發至 `scripts/bootstrap.sh`，若該版本中無法使用則回退至舊版原始碼安裝流程。

若在非 repository checkout 的目錄下執行方式 B，bootstrap 腳本會自動 clone 一個暫存工作區、建置、安裝，最後自動清理。

## 選用初始化模式

### 容器化初始化（Docker）

```bash
./bootstrap.sh --docker
```

此指令會建置本機 ZeroClaw 映像檔，並在容器內啟動初始化流程，同時將設定檔與工作區持久化至 `./.zeroclaw-docker`。

容器 CLI 預設使用 `docker`。若 Docker CLI 不可用但 `podman` 存在，bootstrap 會自動回退至 `podman`。您也可以明確指定 `ZEROCLAW_CONTAINER_CLI`（例如：`ZEROCLAW_CONTAINER_CLI=podman ./bootstrap.sh --docker`）。

使用 Podman 時，bootstrap 會加上 `--userns keep-id` 及 `:Z` volume 標籤，確保工作區與設定掛載在容器內可寫入。

若加上 `--skip-build`，bootstrap 會跳過本機映像建置。它會先嘗試使用本機 Docker 標籤（`ZEROCLAW_DOCKER_IMAGE`，預設為 `zeroclaw-bootstrap:local`）；若不存在，則拉取 `ghcr.io/zeroclaw-labs/zeroclaw:latest` 並標記為本機標籤後再執行。

### 快速初始化（非互動式）

```bash
./bootstrap.sh --onboard --api-key "sk-..." --provider openrouter
```

或透過環境變數：

```bash
ZEROCLAW_API_KEY="sk-..." ZEROCLAW_PROVIDER="openrouter" ./bootstrap.sh --onboard
```

### 互動式初始化

```bash
./bootstrap.sh --interactive-onboard
```

## 實用旗標

- `--install-system-deps`
- `--install-rust`
- `--skip-build`（在 `--docker` 模式下：若有本機映像則使用，否則拉取 `ghcr.io/zeroclaw-labs/zeroclaw:latest`）
- `--skip-install`
- `--provider <id>`

查看所有選項：

```bash
./bootstrap.sh --help
```

## 相關文件

- [README.md](../README.md)
- [commands-reference.md](commands-reference.md)
- [providers-reference.md](providers-reference.md)
- [channels-reference.md](channels-reference.md)
