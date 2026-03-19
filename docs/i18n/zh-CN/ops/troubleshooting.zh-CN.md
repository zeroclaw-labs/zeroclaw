# JhedaiClaw 故障排除

本指南侧重于常见的安装/运行时故障和快速解决路径。

最后验证时间：**2026年2月20日**。

## 安装 / 引导

### 找不到 `cargo`

症状：

- 引导退出，提示 `cargo is not installed`

修复：

```bash
./install.sh --install-rust
```

或从 <https://rustup.rs/> 安装。

### 缺失系统构建依赖

症状：

- 由于编译器或 `pkg-config` 问题导致构建失败

修复：

```bash
./install.sh --install-system-deps
```

### 低内存/低磁盘主机上构建失败

症状：

- `cargo build --release` 被终止（`signal: 9`、OOM 终止器或 `cannot allocate memory`）
- 添加交换空间后构建崩溃，因为磁盘空间耗尽

原因：

- 运行时内存（常规操作 <5MB）与编译时内存不同。
- 完整源码构建可能需要 **2 GB RAM + 交换空间** 和 **6+ GB 可用磁盘**。
- 在小磁盘上启用交换空间可以避免 RAM OOM，但仍可能因磁盘耗尽而失败。

资源受限机器的首选路径：

```bash
./install.sh --prefer-prebuilt
```

仅二进制模式（无源码回退）：

```bash
./install.sh --prebuilt-only
```

如果你必须在资源受限主机上从源码编译：

1. 仅当你有足够的可用磁盘同时容纳交换空间 + 构建输出时才添加交换空间。
2. 限制 cargo 并行度：

```bash
CARGO_BUILD_JOBS=1 cargo build --release --locked
```

3. 不需要 Matrix 时减少重量级功能：

```bash
cargo build --release --locked --features hardware
```

4. 在更强的机器上交叉编译，然后将二进制文件复制到目标主机。

### 构建非常慢或似乎卡住

症状：

- `cargo check` / `cargo build` 似乎长时间卡在 `Checking jhedaiclaw`
- 重复出现 `Blocking waiting for file lock on package cache` 或 `build directory`

JhedaiClaw 中出现此问题的原因：

- Matrix E2EE 栈（`matrix-sdk`、`ruma`、`vodozemac`）很大，类型检查开销高。
- TLS + 加密原生构建脚本（`aws-lc-sys`、`ring`）增加了明显的编译时间。
- 带捆绑 SQLite 的 `rusqlite` 会在本地编译 C 代码。
- 并行运行多个 cargo 任务/工作树会导致锁竞争。

快速检查：

```bash
cargo check --timings
cargo tree -d
```

时间报告写入 `target/cargo-timings/cargo-timing.html`。

更快的本地迭代（不需要 Matrix 渠道时）：

```bash
cargo check
```

这使用精简的默认功能集，可以显著减少编译时间。

要显式启用 Matrix 支持构建：

```bash
cargo check --features channel-matrix
```

要构建支持 Matrix + Lark + 硬件的版本：

```bash
cargo check --features hardware,channel-matrix,channel-lark
```

锁竞争缓解：

```bash
pgrep -af \"cargo (check|build|test)|cargo check|cargo build|cargo test\"
```

在运行自己的构建前停止不相关的 cargo 任务。

### 安装后找不到 `jhedaiclaw` 命令

症状：

- 安装成功，但 shell 找不到 `jhedaiclaw`

修复：

```bash
export PATH=\"$HOME/.cargo/bin:$PATH\"
which jhedaiclaw
```

如有需要，持久化到你的 shell 配置文件中。

## 运行时 / 网关

### 网关不可达

检查：

```bash
jhedaiclaw status
jhedaiclaw doctor
```

验证 `~/.jhedaiclaw/config.toml`：

- `[gateway].host`（默认 `127.0.0.1`）
- `[gateway].port`（默认 `42617`）
- 仅当有意暴露 LAN/公共接口时才设置 `allow_public_bind`

### Webhook 配对 / 认证失败

检查：

1. 确保配对已完成（`/pair` 流程）
2. 确保 bearer 令牌是当前有效的
3. 重新运行诊断：

```bash
jhedaiclaw doctor
```

## 渠道问题

### Telegram 冲突：`terminated by other getUpdates request`

原因：

- 多个轮询器使用同一个机器人令牌

修复：

- 为该令牌仅保留一个活动运行时
- 停止额外的 `jhedaiclaw daemon` / `jhedaiclaw channel start` 进程

### `channel doctor` 中渠道不健康

检查：

```bash
jhedaiclaw channel doctor
```

然后验证配置中特定渠道的凭证 + 白名单字段。

## 服务模式

### 服务已安装但未运行

检查：

```bash
jhedaiclaw service status
```

恢复：

```bash
jhedaiclaw service stop
jhedaiclaw service start
```

Linux 日志：

```bash
journalctl --user -u jhedaiclaw.service -f
```

## 安装程序 URL

```bash
curl -fsSL https://raw.githubusercontent.com/jhedai/jhedaiclaw/master/install.sh | bash
```

## 仍然卡住？

提交 issue 时收集并包含这些输出：

```bash
jhedaiclaw --version
jhedaiclaw status
jhedaiclaw doctor
jhedaiclaw channel doctor
```

同时包含操作系统、安装方法和脱敏的配置片段（无密钥）。

## 相关文档

- [operations-runbook.zh-CN.md](operations-runbook.zh-CN.md)
- [one-click-bootstrap.zh-CN.md](../setup-guides/one-click-bootstrap.zh-CN.md)
- [channels-reference.zh-CN.md](../reference/api/channels-reference.zh-CN.md)
- [network-deployment.zh-CN.md](network-deployment.zh-CN.md)
