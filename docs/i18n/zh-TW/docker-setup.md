# Docker 設定指南

本指南說明如何在 Docker 模式下執行 ZeroClaw，包括初始設定、新手引導和日常使用。

## 前置需求

- [Docker](https://docs.docker.com/engine/install/) 或 [Podman](https://podman.io/getting-started/installation)
- Git

## 快速開始

### 1. 以 Docker 模式啟動初始設定

```bash
# 複製儲存庫
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# 以 Docker 模式執行初始設定
./bootstrap.sh --docker
```

這會建置 Docker 映像檔並準備資料目錄。Docker 模式下**不會**預設執行新手引導。

### 2. 執行新手引導

初始設定完成後，在 Docker 中執行新手引導：

```bash
# 互動式新手引導（首次設定建議使用）
./zeroclaw_install.sh --docker --interactive-onboard

# 或以非互動式搭配 API 金鑰
./zeroclaw_install.sh --docker --api-key "sk-..." --provider openrouter
```

### 3. 啟動 ZeroClaw

#### 常駐服務模式（背景服務）

```bash
# 以背景常駐服務啟動
./zeroclaw_install.sh --docker --docker-daemon

# 檢視日誌
docker logs -f zeroclaw-daemon

# 停止常駐服務
docker rm -f zeroclaw-daemon
```

#### 互動模式

```bash
# 在容器中執行單次指令
docker run --rm -it \
  -v ~/.zeroclaw-docker/.zeroclaw:/home/claw/.zeroclaw \
  -v ~/.zeroclaw-docker/workspace:/workspace \
  zeroclaw-bootstrap:local \
  zeroclaw agent -m "Hello, ZeroClaw!"

# 啟動互動式 CLI 模式
docker run --rm -it \
  -v ~/.zeroclaw-docker/.zeroclaw:/home/claw/.zeroclaw \
  -v ~/.zeroclaw-docker/workspace:/workspace \
  zeroclaw-bootstrap:local \
  zeroclaw agent
```

## 設定

### 資料目錄

Docker 模式預設將資料儲存在：
- `~/.zeroclaw-docker/.zeroclaw/` - 設定檔
- `~/.zeroclaw-docker/workspace/` - 工作區檔案

透過環境變數覆寫：
```bash
ZEROCLAW_DOCKER_DATA_DIR=/custom/path ./bootstrap.sh --docker
```

### 預置設定

如果您已有 `config.toml`，可以在初始設定時預置：

```bash
./bootstrap.sh --docker --docker-config ./my-config.toml
```

### 使用 Podman

```bash
ZEROCLAW_CONTAINER_CLI=podman ./bootstrap.sh --docker
```

## 常用指令

| 任務 | 指令 |
|------|------|
| 啟動常駐服務 | `./zeroclaw_install.sh --docker --docker-daemon` |
| 檢視常駐服務日誌 | `docker logs -f zeroclaw-daemon` |
| 停止常駐服務 | `docker rm -f zeroclaw-daemon` |
| 執行單次 agent | `docker run --rm -it ... zeroclaw agent -m "message"` |
| 互動式 CLI | `docker run --rm -it ... zeroclaw agent` |
| 檢查狀態 | `docker run --rm -it ... zeroclaw status` |
| 啟動頻道 | `docker run --rm -it ... zeroclaw channel start` |

將 `...` 替換為[互動模式](#互動模式)中所示的 volume 掛載參數。

## 重置 Docker 環境

若要完全重置您的 Docker ZeroClaw 環境：

```bash
./bootstrap.sh --docker --docker-reset
```

這會移除：
- Docker 容器
- Docker 網路
- Docker 磁碟區
- 資料目錄（`~/.zeroclaw-docker/`）

## 疑難排解

### "zeroclaw: command not found"

此錯誤發生在嘗試直接在主機上執行 `zeroclaw` 時。在 Docker 模式下，您必須在容器內執行指令：

```bash
# 錯誤（在主機上）
zeroclaw agent

# 正確（在容器內）
docker run --rm -it \
  -v ~/.zeroclaw-docker/.zeroclaw:/home/claw/.zeroclaw \
  -v ~/.zeroclaw-docker/workspace:/workspace \
  zeroclaw-bootstrap:local \
  zeroclaw agent
```

### 初始設定後沒有容器在執行

執行 `./bootstrap.sh --docker` 只會建置映像檔並準備資料目錄。它**不會**啟動容器。要啟動 ZeroClaw：

1. 執行新手引導：`./zeroclaw_install.sh --docker --interactive-onboard`
2. 啟動常駐服務：`./zeroclaw_install.sh --docker --docker-daemon`

### 容器無法啟動

檢查 Docker 日誌以找出錯誤：
```bash
docker logs zeroclaw-daemon
```

常見問題：
- 缺少 API 金鑰：以 `--api-key` 執行新手引導或編輯 `config.toml`
- 權限問題：確保 Docker 有權存取資料目錄

## 環境變數

| 變數 | 說明 | 預設值 |
|------|------|--------|
| `ZEROCLAW_DOCKER_DATA_DIR` | 資料目錄路徑 | `~/.zeroclaw-docker` |
| `ZEROCLAW_DOCKER_IMAGE` | Docker 映像檔名稱 | `zeroclaw-bootstrap:local` |
| `ZEROCLAW_CONTAINER_CLI` | 容器 CLI（docker/podman） | `docker` |
| `ZEROCLAW_DOCKER_DAEMON_NAME` | 常駐服務容器名稱 | `zeroclaw-daemon` |
| `ZEROCLAW_DOCKER_CARGO_FEATURES` | 建置功能特性 | （空白） |

## 相關文件

- [快速開始](../README.md#quick-start)
- [設定參考](config-reference.md)
- [維運手冊](operations-runbook.md)
