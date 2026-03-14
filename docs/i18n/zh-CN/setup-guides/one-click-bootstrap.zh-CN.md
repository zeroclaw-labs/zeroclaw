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

### 快速引导（非交互式）

```bash
./install.sh --onboard --api-key \"sk-...\" --provider openrouter
```

或者使用环境变量：

```bash
ZEROCLAW_API_KEY=\"sk-...\" ZEROCLAW_PROVIDER=\"openrouter\" ./install.sh --onboard
```

### 交互式引导

```bash
./install.sh --interactive-onboard
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

- [README.zh-CN.md](../../../README.zh-CN.md)
- [commands-reference.zh-CN.md](../reference/cli/commands-reference.zh-CN.md)
- [providers-reference.zh-CN.md](../reference/api/providers-reference.zh-CN.md)
- [channels-reference.zh-CN.md](../reference/api/channels-reference.zh-CN.md)
