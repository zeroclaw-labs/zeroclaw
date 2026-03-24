# 一键安装引导

本页面介绍安装和初始化 ZeroClaw 的最快支持路径。

最后验证时间：**2026年2月20日**。

## 选项 0：Homebrew（macOS/Linuxbrew）

```bash
brew install zeroclaw
```

## 选项 A（推荐）：克隆 + 本地脚本

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

默认执行操作：

1. `cargo build --release --locked`
2. `cargo install --path . --force --locked`

### 资源预检和预编译二进制流程

源码编译通常至少需要：

- **2 GB RAM + 交换空间**
- **6 GB 可用磁盘空间**

当资源受限时，安装引导会优先尝试使用预编译二进制文件。

```bash
./install.sh --prefer-prebuilt
```

如果要求仅使用二进制安装，没有兼容的发布资产时直接失败：

```bash
./install.sh --prebuilt-only
```

如果要绕过预编译流程，强制源码编译：

```bash
./install.sh --force-source-build
```

## 双模式引导

默认行为是**仅应用程序**（编译/安装 ZeroClaw），需要已存在 Rust 工具链。

对于全新机器，可以显式启用环境引导：

```bash
./install.sh --install-system-deps --install-rust
```

注意事项：

- `--install-system-deps` 安装编译器/构建依赖（可能需要 `sudo`）。
- `--install-rust` 在缺失时通过 `rustup` 安装 Rust。
- `--prefer-prebuilt` 优先尝试下载发布二进制文件，失败回退到源码编译。
- `--prebuilt-only` 禁用源码回退。
- `--force-source-build` 完全禁用预编译流程。

## 选项 B：远程单行命令

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

对于高安全环境，推荐使用选项 A，这样你可以在执行前审查脚本内容。

如果你在代码仓库外运行选项 B，安装脚本会自动克隆临时工作区，编译、安装，然后清理工作区。

## 可选引导模式

### 容器化引导（Docker）

```bash
./install.sh --docker
```

这会构建本地 ZeroClaw 镜像并在容器内启动引导流程，同时将配置/工作区持久化到 `./.zeroclaw-docker`。

容器 CLI 默认为 `docker`。如果 Docker CLI 不可用且存在 `podman`，安装程序会自动回退到 `podman`。你也可以显式设置 `ZEROCLAW_CONTAINER_CLI`（例如：`ZEROCLAW_CONTAINER_CLI=podman ./install.sh --docker`）。

对于 Podman，安装程序会使用 `--userns keep-id` 和 `:Z` 卷标签，确保工作区/配置挂载在容器内保持可写。

如果你添加 `--skip-build` 参数，安装程序会跳过本地镜像构建。它会首先尝试本地 Docker 标签（`ZEROCLAW_DOCKER_IMAGE`，默认：`zeroclaw-bootstrap:local`）；如果不存在，会拉取 `ghcr.io/zeroclaw-labs/zeroclaw:latest` 并在运行前打本地标签。

### 停止和重启 Docker/Podman 容器

`./install.sh --docker` 完成后，容器会退出。你的配置和工作区会持久保存在数据目录中（默认：`./.zeroclaw-docker`，通过 `curl | bash` 引导时为 `~/.zeroclaw-docker`）。你可以通过 `ZEROCLAW_DOCKER_DATA_DIR` 环境变量覆盖此路径。

**不要重新运行 `install.sh` 来重启** — 这会重建镜像并重新运行引导。取而代之的是，从现有镜像启动一个新容器并挂载已持久化的数据目录。

#### 使用仓库中的 docker-compose.yml

在 Docker/Podman 中长期运行 ZeroClaw 最简单的方式是使用仓库根目录提供的 `docker-compose.yml`。它使用命名卷（`zeroclaw-data`）并设置 `restart: unless-stopped`，因此容器可以在重启后自动恢复。

```bash
# 启动（后台运行，重启自动恢复）
docker compose up -d

# 停止
docker compose down

# 停止后重启
docker compose up -d
```

如果你使用 Podman，将 `docker` 替换为 `podman`。

#### 手动容器运行（使用 install.sh 数据目录）

如果你通过 `./install.sh --docker` 安装并且想在不使用 compose 的情况下重用 `.zeroclaw-docker` 数据目录：

```bash
# Docker
docker run -d --name zeroclaw \
  --restart unless-stopped \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway

# Podman（添加 --userns keep-id 和 :Z 卷标签）
podman run -d --name zeroclaw \
  --restart unless-stopped \
  --userns keep-id \
  --user "$(id -u):$(id -g)" \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw:Z" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace:Z" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway
```

#### 常用生命周期命令

```bash
# 停止容器（保留数据）
docker stop zeroclaw

# 启动已停止的容器（配置和工作区保持完整）
docker start zeroclaw

# 查看日志
docker logs -f zeroclaw

# 删除容器（数据保存在 volumes/.zeroclaw-docker 中保留）
docker rm zeroclaw

# 检查健康状态
docker exec zeroclaw zeroclaw status
```

#### 环境变量

手动运行时，如果提供者配置已经保存在持久化的 `config.toml` 中，则不需要传递提供者配置环境变量：

```bash
docker run -d --name zeroclaw \
  -e API_KEY="sk-..." \
  -e PROVIDER="openrouter" \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway
```

如果你在初始安装期间已经运行了 `onboard`，你的 API 密钥和提供者已经保存到 `.zeroclaw-docker/.zeroclaw/config.toml`，不需要再次传递。

### 快速引导（非交互式）

```bash
./install.sh --api-key \"sk-...\" --provider openrouter
```

或者使用环境变量：

```bash
ZEROCLAW_API_KEY=\"sk-...\" ZEROCLAW_PROVIDER=\"openrouter\" ./install.sh
```

## 有用的参数

- `--install-system-deps`
- `--install-rust`
- `--skip-build`（在 `--docker` 模式下：如果存在使用本地镜像，否则拉取 `ghcr.io/zeroclaw-labs/zeroclaw:latest`）
- `--skip-install`
- `--provider <id>`

查看所有选项：

```bash
./install.sh --help
```

## 相关文档

- [README.md](../../../README.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
- [providers-reference.md](../reference/api/providers-reference.md)
- [channels-reference.md](../reference/api/channels-reference.md)
