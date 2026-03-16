# macOS 更新与卸载指南

本页面记录了 macOS（OS X）上 ZeroClaw 支持的更新和卸载流程。

最后验证时间：**2026年2月22日**。

## 1) 检查当前安装方式

```bash
which zeroclaw
zeroclaw --version
```

典型安装位置：

- Homebrew：`/opt/homebrew/bin/zeroclaw`（Apple Silicon）或 `/usr/local/bin/zeroclaw`（Intel）
- Cargo/引导安装/手动安装：`~/.cargo/bin/zeroclaw`

如果两者都存在，由你的 shell `PATH` 顺序决定运行哪一个。

## 2) 在 macOS 上更新

### A) Homebrew 安装

```bash
brew update
brew upgrade zeroclaw
zeroclaw --version
```

### B) 克隆 + 引导安装

在你本地的代码仓库目录中执行：

```bash
git pull --ff-only
./install.sh --prefer-prebuilt
zeroclaw --version
```

如果你想要仅源码更新：

```bash
git pull --ff-only
cargo install --path . --force --locked
zeroclaw --version
```

### C) 手动预编译二进制安装

使用最新的发布资产重新运行你的下载/安装流程，然后验证：

```bash
zeroclaw --version
```

## 3) 在 macOS 上卸载

### A) 首先停止并移除后台服务

这可以防止守护进程在二进制文件被移除后继续运行。

```bash
zeroclaw service stop || true
zeroclaw service uninstall || true
```

`service uninstall` 会移除的服务文件：

- `~/Library/LaunchAgents/com.zeroclaw.daemon.plist`

### B) 根据安装方式移除二进制文件

Homebrew：

```bash
brew uninstall zeroclaw
```

Cargo/引导安装/手动安装（`~/.cargo/bin/zeroclaw`）：

```bash
cargo uninstall zeroclaw || true
rm -f ~/.cargo/bin/zeroclaw
```

### C) 可选：移除本地运行时数据

仅当你想要完全清理配置、认证配置文件、日志和工作区状态时运行此命令。

```bash
rm -rf ~/.zeroclaw
```

## 4) 验证卸载完成

```bash
command -v zeroclaw || echo \"zeroclaw 二进制文件未找到\"
pgrep -fl zeroclaw || echo \"没有运行中的 zeroclaw 进程\"
```

如果 `pgrep` 仍然找到进程，手动停止它并重新检查：

```bash
pkill -f zeroclaw
```

## 相关文档

- [一键安装引导](one-click-bootstrap.zh-CN.md)
- [命令参考](../reference/cli/commands-reference.zh-CN.md)
- [故障排除](../ops/troubleshooting.zh-CN.md)
