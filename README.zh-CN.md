<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀（简体中文）</h1>

<p align="center">
  <strong>零开销、零妥协；随处部署、万物可换。</strong>
</p>

<p align="center">
  🌐 语言：<a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ru.md">Русский</a>
</p>

<p align="center">
  <a href="bootstrap.sh">一键部署</a> |
  <a href="docs/getting-started/README.md">安装入门</a> |
  <a href="docs/README.zh-CN.md">文档总览</a> |
  <a href="docs/SUMMARY.md">文档目录</a>
</p>

<p align="center">
  <strong>场景分流：</strong>
  <a href="docs/reference/README.md">参考手册</a> ·
  <a href="docs/operations/README.md">运维部署</a> ·
  <a href="docs/troubleshooting.md">故障排查</a> ·
  <a href="docs/security/README.md">安全专题</a> ·
  <a href="docs/hardware/README.md">硬件外设</a> ·
  <a href="docs/contributing/README.md">贡献与 CI</a>
</p>

> 本文是对 `README.md` 的人工对齐翻译（强调可读性与准确性，不做逐字直译）。
> 
> 技术标识（命令、配置键、API 路径、Trait 名称）保持英文，避免语义漂移。
> 
> 最后对齐时间：**2026-02-18**。

## 项目简介

ZeroClaw 是一个高性能、低资源占用、可组合的自主智能体运行时：

- Rust 原生实现，单二进制部署，跨 ARM / x86 / RISC-V。
- Trait 驱动架构，`Provider` / `Channel` / `Tool` / `Memory` 可替换。
- 安全默认值优先：配对鉴权、显式 allowlist、沙箱与作用域约束。

## 为什么选择 ZeroClaw

- **默认轻量运行时**：常见 CLI 与 `status` 工作流通常保持在几 MB 级内存范围。
- **低成本部署友好**：面向低价板卡与小规格云主机设计，不依赖厚重运行时。
- **冷启动很快**：Rust 单二进制让常用命令与守护进程启动更接近“秒开”。
- **跨架构可移植**：同一套二进制优先流程覆盖 ARM / x86 / RISC-V，并保持 provider/channel/tool 可替换。

## 基准快照（ZeroClaw vs OpenClaw，可复现）

以下是本地快速基准对比（macOS arm64，2026 年 2 月），按 0.8GHz 边缘 CPU 进行归一化展示：

| | OpenClaw | NanoBot | PicoClaw | ZeroClaw 🦀 |
|---|---|---|---|---|
| **语言** | TypeScript | Python | Go | **Rust** |
| **RAM** | > 1GB | > 100MB | < 10MB | **< 5MB** |
| **启动时间（0.8GHz 核）** | > 500s | > 30s | < 1s | **< 10ms** |
| **二进制体积** | ~28MB（dist） | N/A（脚本） | ~8MB | **3.4 MB** |
| **成本** | Mac Mini $599 | Linux SBC ~$50 | Linux 板卡 $10 | **任意 $10 硬件** |

> 说明：该表用于方向性对比，结果会随环境变化。OpenClaw 依赖 Node.js 运行时（额外开销明显）；NanoBot 依赖 Python 运行时；PicoClaw 与 ZeroClaw 为静态二进制。

<p align="center">
  <img src="zero-claw.jpeg" alt="ZeroClaw vs OpenClaw 对比图" width="800" />
</p>

### 本地可复现测量

基准数据会随代码与工具链变化，建议始终在你的目标环境自行复测：

```bash
cargo build --release
ls -lh target/release/zeroclaw

/usr/bin/time -l target/release/zeroclaw --help
/usr/bin/time -l target/release/zeroclaw status
```

当前 README 的样例数据（macOS arm64，2026-02-18）：

- Release 二进制：`8.8M`
- `zeroclaw --help`：约 `0.02s`，峰值内存约 `3.9MB`
- `zeroclaw status`：约 `0.01s`，峰值内存约 `4.1MB`

## 一键部署

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

可选环境初始化：`./bootstrap.sh --install-system-deps --install-rust`（可能需要 `sudo`）。

详细说明见：[`docs/one-click-bootstrap.md`](docs/one-click-bootstrap.md)。

## 快速开始

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release --locked
cargo install --path . --force --locked

# 快速初始化（无交互）
zeroclaw onboard --api-key sk-... --provider openrouter

# 或使用交互式向导
zeroclaw onboard --interactive

# 单次对话
zeroclaw agent -m "Hello, ZeroClaw!"

# 启动网关（默认: 127.0.0.1:3000）
zeroclaw gateway

# 启动长期运行模式
zeroclaw daemon
```

## 安全默认行为（关键）

- Gateway 默认绑定：`127.0.0.1:3000`
- Gateway 默认要求配对：`require_pairing = true`
- 默认拒绝公网绑定：`allow_public_bind = false`
- Channel allowlist 语义：
  - 空列表 `[]` => deny-by-default
  - `"*"` => allow all（仅在明确知道风险时使用）

## 常用配置片段

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.7

[memory]
backend = "sqlite"             # sqlite | lucid | markdown | none
auto_save = true
embedding_provider = "none"    # none | openai | custom:https://...

[gateway]
host = "127.0.0.1"
port = 3000
require_pairing = true
allow_public_bind = false
```

## 文档导航（推荐从这里开始）

- 文档总览（英文）：[`docs/README.md`](docs/README.md)
- 统一目录（TOC）：[`docs/SUMMARY.md`](docs/SUMMARY.md)
- 文档总览（简体中文）：[`docs/README.zh-CN.md`](docs/README.zh-CN.md)
- 命令参考：[`docs/commands-reference.md`](docs/commands-reference.md)
- 配置参考：[`docs/config-reference.md`](docs/config-reference.md)
- Provider 参考：[`docs/providers-reference.md`](docs/providers-reference.md)
- Channel 参考：[`docs/channels-reference.md`](docs/channels-reference.md)
- 运维手册：[`docs/operations-runbook.md`](docs/operations-runbook.md)
- 故障排查：[`docs/troubleshooting.md`](docs/troubleshooting.md)
- 文档清单与分类：[`docs/docs-inventory.md`](docs/docs-inventory.md)
- 项目 triage 快照（2026-02-18）：[`docs/project-triage-snapshot-2026-02-18.md`](docs/project-triage-snapshot-2026-02-18.md)

## 贡献与许可证

- 贡献指南：[`CONTRIBUTING.md`](CONTRIBUTING.md)
- PR 工作流：[`docs/pr-workflow.md`](docs/pr-workflow.md)
- Reviewer 指南：[`docs/reviewer-playbook.md`](docs/reviewer-playbook.md)
- 许可证：MIT（见 [`LICENSE`](LICENSE) 与 [`NOTICE`](NOTICE)）

---

如果你需要完整实现细节（架构图、全部命令、完整 API、开发流程），请直接阅读英文主文档：[`README.md`](README.md)。
