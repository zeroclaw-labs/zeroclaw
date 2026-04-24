# ZeroClaw 项目开发指南

## 目录

1. [根目录文件/文件夹解析](#1-根目录文件文件夹解析)
2. [Rust 项目组织架构分析](#2-rust-项目组织架构分析)
3. [测试框架与运行方式](#3-测试框架与运行方式)
4. [快速开始](#4-快速开始)

---

## 1. 根目录文件/文件夹解析

### 1.1 核心目录结构

```
zeroclaw/
├── apps/                    # 应用程序目录
├── benches/                 # 性能基准测试
├── crates/                  # Rust 工作空间 crate（核心子项目）
├── deploy-k8s/              # Kubernetes 部署配置
├── dev/                     # 开发工具和脚本
├── dist/                    # 发行包配置（AUR、Scoop）
├── docs/                    # 项目文档
├── firmware/                # 嵌入式固件代码
├── fuzz/                    # 模糊测试目标
├── marketplace/             # 应用市场部署配置
├── scripts/                 # 辅助脚本
├── src/                     # 主应用源代码
├── tests/                   # 集成测试和系统测试
├── tool_descriptions/       # 工具描述多语言文件
└── web/                     # Web 前端（React + Vite）
```

### 2.2 详细目录说明

#### `apps/` - 应用程序目录

| 目录 | 描述 |
|------|------|
| `tauri/` | Tauri 桌面应用程序，提供原生桌面界面 |

#### `benches/` - 性能基准测试

| 文件 | 描述 |
|------|------|
| `agent_benchmarks.rs` | Agent 性能基准测试，使用 Criterion 框架 |

#### `crates/` - Rust 工作空间 Crate

这是项目的核心，包含 18 个独立的 Rust crate，详见 [第 3 节](#3-rust-项目组织架构分析)。

#### `deploy-k8s/` - Kubernetes 部署

| 文件 | 描述 |
|------|------|
| `configmap-sample.yaml` | ConfigMap 配置示例 |
| `deployment-sample.yaml` | Deployment 部署示例 |
| `namespace-sample.yaml` | Namespace 命名空间示例 |
| `route-sample.yaml` | OpenShift Route 路由示例 |
| `secret-sample.yaml` | Secret 密钥示例 |
| `service-sample.yaml` | Service 服务示例 |

#### `dev/` - 开发工具

| 目录/文件 | 描述 |
|-----------|------|
| `ci/` | CI Docker 配置 |
| `sandbox/` | 沙箱环境 Docker 配置 |
| `ci.sh` | 本地 CI 测试脚本（核心开发工具） |
| `cli.sh` | CLI 测试辅助脚本 |
| `config.harness-test.toml` | 测试配置模板 |
| `config.template.toml` | 配置模板 |
| `docker-compose*.yml` | Docker Compose 配置 |
| `kill-port.py` | 端口清理脚本 |
| `test-*.sh` | 各种测试脚本 |

#### `dist/` - 发行包配置

| 目录 | 描述 |
|------|------|
| `aur/` | Arch Linux AUR 包构建配置 |
| `scoop/` | Windows Scoop 包管理器配置 |

#### `docs/` - 项目文档

| 目录 | 描述 |
|------|------|
| `architecture/` | 架构决策记录 (ADR) |
| `assets/` | 文档资源（图片、图表） |
| `contributing/` | 贡献指南 |
| `foundations/` | 项目基础文档 |
| `getting-started/` | 入门指南 |
| `hardware/` | 硬件相关文档 |
| `i18n/` | 多语言文档（30+ 语言） |
| `maintainers/` | 维护者指南 |
| `ops/` | 运维文档 |
| `reference/` | API 参考文档 |
| `security/` | 安全文档 |
| `setup-guides/` | 安装配置指南 |
| `superpowers/` | 功能规格文档 |

#### `firmware/` - 嵌入式固件

| 目录 | 描述 |
|------|------|
| `arduino/` | Arduino 固件（.ino 文件） |
| `esp32/` | ESP32 Rust 固件 |
| `esp32-ui/` | ESP32 带显示界面的固件 |
| `nucleo/` | STM32 Nucleo 固件 |
| `pico/` | Raspberry Pi Pico 固件 |
| `uno-q-bridge/` | Arduino Uno Q 桥接固件 |
| `zeroclaw-fw-protocol/` | 固件通信协议库 |

#### `fuzz/` - 模糊测试

| 文件 | 描述 |
|------|------|
| `fuzz_targets/fuzz_config_parse.rs` | 配置解析模糊测试 |
| `fuzz_targets/fuzz_tool_params.rs` | 工具参数模糊测试 |

#### `marketplace/` - 应用市场

| 目录 | 描述 |
|------|------|
| `coolify/` | Coolify 部署配置 |
| `dokploy/` | Dokploy 部署配置 |
| `easypanel/` | Easypanel 部署配置 |

#### `scripts/` - 辅助脚本

| 目录/文件 | 描述 |
|-----------|------|
| `browser/` | 浏览器相关脚本（VNC 启动/停止） |
| `ci/` | CI 质量检查脚本 |
| `release/` | 版本发布脚本 |
| `deploy-rpi.sh` | Raspberry Pi 部署脚本 |
| `rpi-config.toml` | Raspberry Pi 配置模板 |
| `zeroclaw.service` | Systemd 服务文件 |

#### `src/` - 主应用源代码

这是主二进制 `zeroclaw` 的源代码，详见 [2.4 节](#24-主应用-src)。

#### `tests/` - 测试目录

| 目录 | 描述 |
|------|------|
| `component/` | 组件测试 |
| `fixtures/` | 测试资源文件 |
| `integration/` | 集成测试 |
| `live/` | 在线测试（需要真实 API） |
| `manual/` | 手动测试脚本 |
| `support/` | 测试支持代码（Mock、辅助函数） |
| `system/` | 系统测试 |

#### `tool_descriptions/` - 工具描述多语言

包含 30+ 语言的工具描述 `.toml` 文件，用于国际化支持。

#### `web/` - Web 前端

| 目录/文件 | 描述 |
|-----------|------|
| `public/` | 静态资源 |
| `src/` | React 源代码 |
| `src/components/` | React 组件 |
| `src/contexts/` | React Context |
| `src/hooks/` | React Hooks |
| `src/lib/` | 工具库 |
| `src/pages/` | 页面组件 |
| `src/types/` | TypeScript 类型定义 |
| `package.json` | NPM 配置 |
| `vite.config.ts` | Vite 构建配置 |

### 1.2 根目录核心文件

| 文件 | 描述 |
|------|------|
| `Cargo.toml` | Rust 工作空间配置（核心） |
| `Cargo.lock` | 依赖锁定文件 |
| `src/main.rs` | CLI 入口点 |
| `src/lib.rs` | 库入口点 |
| `build.rs` | 构建脚本 |
| `AGENTS.md` | AI 助手指令文档 |
| `README.md` | 项目主 README |
| `CHANGELOG-next.md` | 下版本变更日志 |
| `SECURITY.md` | 安全政策 |
| `CODE_OF_CONDUCT.md` | 行为准则 |
| `CONTRIBUTING.md` | 贡献指南 |
| `CLAUDE.md` | Claude 特定指令 |
| `LICENSE-APACHE` | Apache 2.0 许可证 |
| `LICENSE-MIT` | MIT 许可证 |
| `NOTICE` | 版权声明 |
| `Justfile` | Just 命令运行器配置 |
| `rustfmt.toml` | Rust 格式化配置 |
| `clippy.toml` | Clippy 配置 |
| `deny.toml` | Cargo deny 配置 |
| `taplo.toml` | TOML 格式化配置 |
| `Dockerfile` | 主 Dockerfile |
| `Dockerfile.ci` | CI Dockerfile |
| `Dockerfile.debian` | Debian Dockerfile |
| `docker-compose.yml` | Docker Compose 配置 |
| `flake.nix` | Nix 包管理配置 |
| `flake.lock` | Nix 锁定文件 |
| `install.sh` | 一键安装脚本 |
| `setup.bat` | Windows 安装脚本 |
| `rustup-init.exe` | Rust 安装程序（Windows） |
| `release-plz.toml` | Release-plz 配置 |

---

## 2. Rust 项目组织架构分析

### 2.1 工作空间概述

ZeroClaw 是一个 **Rust Cargo 工作空间（Workspace）**，由根目录的 `Cargo.toml` 定义：

```toml
[workspace]
members = [
    ".",                          # 主 crate
    "crates/zeroclaw-api",        # 公共 API
    "crates/zeroclaw-infra",      # 基础设施
    "crates/zeroclaw-config",     # 配置管理
    "crates/zeroclaw-providers",  # 模型提供者
    "crates/zeroclaw-memory",     # 记忆系统
    "crates/zeroclaw-channels",   # 消息渠道
    "crates/zeroclaw-tools",      # 工具执行
    "crates/zeroclaw-runtime",    # Agent 运行时
    "crates/zeroclaw-tui",        # 终端界面
    "crates/zeroclaw-plugins",    # WASM 插件
    "crates/zeroclaw-gateway",    # 网关服务
    "crates/zeroclaw-hardware",   # 硬件支持
    "crates/zeroclaw-tool-call-parser",  # 工具调用解析
    "crates/robot-kit",           # 机器人工具包
    "crates/aardvark-sys",        # Aardvark FFI
    "crates/zeroclaw-macros",     # 过程宏
    "apps/tauri"                   # Tauri 桌面应用
]
```

### 2.2 项目总览

| 类别 | 数量 | 说明 |
|------|------|------|
| **工作空间 Crate** | 18 个 | `crates/` 目录下的独立库 |
| **主应用** | 1 个 | 根目录 `Cargo.toml` 定义的主二进制 |
| **固件 Crate** | 6 个 | `firmware/` 目录下的嵌入式项目 |
| **模糊测试** | 1 个 | `fuzz/` 目录 |
| **桌面应用** | 1 个 | `apps/tauri/` |

**总计：27 个 Rust 项目（含工作空间内外）**

### 2.3 工作空间 Crate 详细说明

#### 核心层

| Crate | 稳定性 | 描述 |
|-------|--------|------|
| `zeroclaw-api` | Experimental | **公共 trait 定义** - Provider、Channel、Tool、Memory、Observer、Peripheral、RuntimeAdapter 等核心 trait |
| `zeroclaw-infra` | Beta | **共享基础设施** - 防抖（debounce）、会话存储（session）、停滞看门狗（stall watchdog） |
| `zeroclaw-macros` | Beta | **过程宏** - `Configurable` 派生宏，用于配置解析 |

#### 服务层

| Crate | 稳定性 | 描述 |
|-------|--------|------|
| `zeroclaw-config` | Beta | **配置管理** - 配置加载、合并、验证、Schema 导出、成本追踪、安全策略 |
| `zeroclaw-providers` | Beta | **模型提供者** - 20+ LLM 后端（OpenAI、Anthropic、Gemini、Ollama、GLM 等）、弹性包装器、故障转移、路由 |
| `zeroclaw-memory` | Beta | **记忆系统** - Markdown、SQLite、向量嵌入、Qdrant、知识图谱、记忆衰减、冲突解决 |

#### 功能层

| Crate | 稳定性 | 描述 |
|-------|--------|------|
| `zeroclaw-channels` | Experimental | **消息渠道** - 30+ 消息平台集成（WhatsApp、Telegram、Discord、Slack、WeChat 等）、编排器、媒体管道 |
| `zeroclaw-tools` | Experimental | **工具执行** - 70+ 工具（Shell、文件、浏览器、Git、MCP、Jira、Notion 等） |
| `zeroclaw-gateway` | Experimental | **网关服务** - HTTP/WS/SSE 服务器、Web 仪表板、API 端点、TLS、会话管理 |

#### 运行时层

| Crate | 稳定性 | 描述 |
|-------|--------|------|
| `zeroclaw-runtime` | Experimental | **Agent 运行时** - Agent 循环、安全沙箱、Cron 调度、SOP 工作流、Skills 系统、可观测性、WebAuthn |
| `zeroclaw-tui` | Experimental | **终端界面** - TUI 向导弹出、Ratatui + Crossterm |
| `zeroclaw-plugins` | Experimental | **WASM 插件** - WebAssembly 插件系统、插件协议、签名验证 |
| `zeroclaw-hardware` | Experimental | **硬件支持** - USB 发现、外设管理、GPIO、串口、UF2 烧录、ESP32/STM32/RPi 支持 |
| `zeroclaw-tool-call-parser` | Beta | **工具调用解析** - LLM 工具调用语法解析器 |

#### 专用层

| Crate | 稳定性 | 描述 |
|-------|--------|------|
| `robot-kit` | Experimental | **机器人工具包** - 机器人控制库（配置、驱动、表情、监听、视觉、安全、感知、语音） |
| `aardvark-sys` | Experimental | **Aardvark FFI** - Total Phase Aardvark I2C/SPI/GPIO 适配器的 Rust 绑定 |

### 2.4 主应用（src/）

主应用 `zeroclaw` 二进制的源代码位于 `src/` 目录：

```
src/
├── agent/           # Agent 模块
├── approval/        # 审批模块
├── auth/            # 认证模块
├── channels/        # 渠道实现（主应用中的渠道）
├── commands/        # CLI 命令
├── config/          # 配置模块
├── cost/            # 成本追踪
├── cron/            # Cron 调度
├── daemon/          # 守护进程
├── doctor/          # 诊断工具
├── gateway/         # 网关模块
├── hands/           # 多 Agent 编排
├── hardware/        # 硬件模块
├── health/          # 健康检查
├── heartbeat/       # 心跳
├── hooks/           # 生命周期钩子
├── integrations/    # 集成
├── memory/          # 记忆模块
├── nodes/           # 节点模块
├── observability/   # 可观测性
├── onboard/         # 向导弹出
├── peripherals/     # 外设
├── platform/        # 平台适配
├── plugins/         # 插件
├── providers/       # 提供者
├── rag/             # RAG
├── routines/        # 例程
├── security/        # 安全
├── service/         # 服务管理
├── skillforge/      # 技能锻造
├── skills/          # 技能系统
├── sop/             # SOP
├── tools/           # 工具
├── trust/           # 信任
├── tui/             # TUI
├── tunnel/          # 隧道
├── verifiable_intent/  # 可验证意图
├── cli_input.rs     # CLI 输入处理
├── i18n.rs          # 国际化
├── identity.rs      # 身份
├── lib.rs           # 库入口
├── main.rs          # 二进制入口
├── migration.rs     # 迁移
├── multimodal.rs    # 多模态
└── util.rs          # 工具函数
```

### 2.5 固件项目

| 项目 | 目标平台 | 描述 |
|------|----------|------|
| `firmware/esp32/` | ESP32 | 无线外设 Agent |
| `firmware/esp32-ui/` | ESP32 + 显示 | 带可视化界面的 Agent |
| `firmware/nucleo/` | STM32 Nucleo | 工业外设 |
| `firmware/pico/` | Raspberry Pi Pico | 微控制器外设 |
| `firmware/zeroclaw-fw-protocol/` | 通用 | 固件通信协议库 |

### 2.6 依赖关系图（简化）

```
                    ┌─────────────────┐
                    │   zeroclaw-api  │
                    │  (公共 Trait)   │
                    └────────┬────────┘
                             │
        ┌────────────────────┼────────────────────┐
        │                    │                    │
        ▼                    ▼                    ▼
┌───────────────┐   ┌───────────────┐   ┌───────────────┐
│ zeroclaw-infra│   │zeroclaw-macros│   │zeroclaw-config│
│  (基础设施)   │   │  (过程宏)     │   │  (配置管理)   │
└───────┬───────┘   └───────────────┘   └───────┬───────┘
        │                                          │
        └──────────────────┬───────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
        ▼                  ▼                  ▼
┌───────────────┐   ┌───────────────┐   ┌───────────────┐
│zeroclaw-      │   │zeroclaw-      │   │zeroclaw-      │
│providers      │   │memory         │   │tool-call-parser│
│(模型提供者)   │   │(记忆系统)     │   │(工具解析)     │
└───────┬───────┘   └───────┬───────┘   └───────────────┘
        │                   │
        └─────────┬─────────┘
                  │
        ┌─────────┼─────────┐
        │         │         │
        ▼         ▼         ▼
┌──────────┐ ┌──────────┐ ┌──────────┐
│zeroclaw- │ │zeroclaw- │ │zeroclaw- │
│channels  │ │tools     │ │gateway   │
│(消息渠道)│ │(工具执行)│ │(网关服务)│
└────┬─────┘ └────┬─────┘ └────┬─────┘
     │             │             │
     └─────────────┼─────────────┘
                   │
                   ▼
           ┌───────────────┐
           │ zeroclaw-     │
           │ runtime       │
           │ (Agent运行时) │
           └───────┬───────┘
                   │
    ┌──────────────┼──────────────┐
    │              │              │
    ▼              ▼              ▼
┌──────────┐ ┌──────────┐ ┌──────────┐
│zeroclaw- │ │zeroclaw- │ │zeroclaw- │
│tui       │ │plugins   │ │hardware  │
│(终端界面)│ │(WASM插件)│ │(硬件支持)│
└──────────┘ └──────────┘ └──────────┘
```

---

## 3. 测试框架与运行方式

### 3.1 五级测试分类

ZeroClaw 使用五级测试分类体系：

| 级别 | 名称 | 测试内容 | 外部边界 | 目录位置 |
|------|------|----------|----------|----------|
| **L1** | Unit（单元） | 单个函数/结构体 | 全部 Mock | `src/**/*.rs` 中的 `#[cfg(test)]` |
| **L2** | Component（组件） | 单个子系统 | 子系统真实，其他 Mock | `tests/component/` |
| **L3** | Integration（集成） | 多个内部组件协作 | 内部真实，外部 API Mock | `tests/integration/` |
| **L4** | System（系统） | 完整请求→响应流程 | 仅外部 API Mock | `tests/system/` |
| **L5** | Live（在线） | 完整真实服务 | 无 Mock（需 `#[ignore]`） | `tests/live/` |

### 3.2 测试目录结构

```
tests/
├── component/           # L2 组件测试
│   ├── config_migration.rs
│   ├── config_persistence.rs
│   ├── config_schema.rs
│   ├── dockerignore_test.rs
│   ├── gateway.rs
│   ├── gemini_capabilities.rs
│   ├── mod.rs
│   ├── provider_resolution.rs
│   ├── provider_schema.rs
│   └── security.rs
├── integration/         # L3 集成测试
│   ├── agent.rs
│   ├── agent_robustness.rs
│   ├── channel_matrix.rs
│   ├── channel_routing.rs
│   ├── email_attachments.rs
│   ├── hooks.rs
│   ├── memory_comparison.rs
│   ├── memory_restart.rs
│   └── mod.rs
├── system/              # L4 系统测试
│   ├── full_stack.rs
│   └── mod.rs
├── live/                # L5 在线测试
│   ├── mod.rs
│   ├── openai_codex_vision_e2e.rs
│   ├── providers.rs
│   └── zai_jwt_auth.rs
├── support/             # 测试支持代码
│   ├── assertions.rs
│   ├── helpers.rs
│   ├── mock_channel.rs
│   ├── mock_provider.rs
│   ├── mock_tools.rs
│   ├── mod.rs
│   └── trace.rs
├── fixtures/            # 测试资源
│   ├── hello.mp3
│   ├── test_document.pdf
│   └── test_photo.jpg
├── manual/              # 手动测试脚本
│   ├── telegram/
│   ├── tmux/
│   └── test_dockerignore.sh
├── test_component.rs    # 组件测试入口
├── test_integration.rs  # 集成测试入口
├── test_live.rs         # 在线测试入口
└── test_system.rs       # 系统测试入口
```

### 3.3 测试支持模块

| 模块 | 内容 |
|------|------|
| `mock_provider.rs` | `MockProvider`（FIFO 脚本化）、`RecordingProvider`（捕获请求）、`TraceLlmProvider`（JSON 回放） |
| `mock_tools.rs` | `EchoTool`、`CountingTool`、`FailingTool`、`RecordingTool` |
| `mock_channel.rs` | `TestChannel`（捕获发送、记录打字事件） |
| `helpers.rs` | `make_memory()`、`make_observer()`、`build_agent()`、`text_response()` 等辅助函数 |
| `trace.rs` | `LlmTrace` 类型 + `LlmTrace::from_file()` 加载器 |
| `assertions.rs` | `verify_expects()` 声明式断言 |

### 3.4 运行测试命令

#### 基础命令

```bash
# 运行所有测试（单元 + 组件 + 集成 + 系统）
cargo test

# 仅运行单元测试
cargo test --lib

# 运行组件测试
cargo test --test component

# 运行集成测试
cargo test --test integration

# 运行系统测试
cargo test --test system

# 运行在线测试（需要 API 凭据）
cargo test --test live -- --ignored

# 过滤特定测试
cargo test --test integration agent

# 显示详细输出
cargo test -- --nocapture
```

#### 使用开发脚本

```bash
# 完整 CI 验证（推荐）
./dev/ci.sh all

# 分级测试
./dev/ci.sh test-component
./dev/ci.sh test-integration
./dev/ci.sh test-system
./dev/ci.sh test-live

# 代码质量检查
./dev/ci.sh lint
./dev/ci.sh lint-strict

# 安全检查
./dev/ci.sh security
```

#### 预提交检查（推荐）

```bash
# 格式化检查
cargo fmt --all -- --check

# Clippy 检查
cargo clippy --all-targets -- -D warnings

# 运行测试
cargo test
```

### 3.5 JSON Trace Fixtures

Trace fixtures 是存储为 JSON 文件的预制 LLM 响应脚本，用于声明式测试：

**位置**：`tests/fixtures/traces/`

**格式示例**：

```json
{
  "model_name": "test-name",
  "turns": [
    {
      "user_input": "User message",
      "steps": [
        {
          "response": {
            "type": "text",
            "content": "LLM response",
            "input_tokens": 20,
            "output_tokens": 10
          }
        }
      ]
    }
  ],
  "expects": {
    "response_contains": ["expected text"],
    "tools_used": ["echo"],
    "max_tool_calls": 1
  }
}
```

**可用断言字段**：
- `response_contains` - 响应包含文本
- `response_not_contains` - 响应不包含文本
- `tools_used` - 使用的工具列表
- `tools_not_used` - 未使用的工具列表
- `max_tool_calls` - 最大工具调用次数
- `all_tools_succeeded` - 所有工具成功
- `response_matches` - 正则匹配

---

## 4. 快速开始

### 4.1 环境准备

#### Windows 11

```powershell
# 1. 安装 Visual Studio Build Tools
winget install Microsoft.VisualStudio.2022.BuildTools

# 2. 安装 Rust
winget install Rustlang.Rustup
rustup default stable

# 3. 验证
rustc --version
cargo --version
```

#### Linux / macOS

```bash
# 1. 安装构建工具
# Ubuntu/Debian:
sudo apt install build-essential pkg-config

# macOS:
xcode-select --install

# 2. 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 3. 验证
rustc --version
cargo --version
```

### 4.2 克隆并构建

```bash
# 克隆仓库
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# 构建（默认特性）
cargo build --release

# 或构建完整特性
cargo build --release --features agent-runtime,channel-matrix,channel-lark

# 安装到系统
cargo install --path . --force
```

### 4.3 运行测试

```bash
# 运行所有测试
cargo test

# 或使用完整 CI 脚本
./dev/ci.sh all
```

### 4.4 常用命令

```bash
# 查看版本
zeroclaw --version

# 向导弹出配置
zeroclaw onboard

# 启动网关
zeroclaw gateway

# 与 Agent 对话
zeroclaw agent -m "Hello, ZeroClaw!"

# 诊断系统状态
zeroclaw doctor

# 查看状态
zeroclaw status
```

---

## 附录

### A. 特性标志

| 特性 | 描述 | 默认 |
|------|------|------|
| `agent-runtime` | 完整 Agent 运行时 | ✅ |
| `observability-prometheus` | Prometheus 指标 | ✅ |
| `schema-export` | JSON Schema 导出 | ✅ |
| `channel-matrix` | Matrix 协议 | ❌ |
| `channel-lark` | Lark/Feishu | ❌ |
| `channel-nostr` | Nostr 协议 | ❌ |
| `browser-native` | 无头浏览器 | ❌ |
| `hardware` | USB 设备支持 | ❌ |
| `rag-pdf` | PDF 提取 | ❌ |
| `observability-otel` | OpenTelemetry | ❌ |
| `plugins-wasm` | WASM 插件 | ❌ |

### B. 构建配置

```toml
# 根 Cargo.toml 中的构建配置
[profile.release]
opt-level = "z"      # 优化大小
lto = "fat"           # 最大跨 crate 优化
codegen-units = 1    # 低内存设备优化
strip = true          # 移除调试符号
panic = "abort"       # 减小二进制

[profile.release-fast]
inherits = "release"
codegen-units = 8     # 并行构建（高性能机器）
```

### C. 相关文档

- [项目 README](../README.md)
- [贡献指南](../CONTRIBUTING.md)
- [安全政策](../SECURITY.md)
- [Windows 安装指南](../setup-guides/windows-setup.md)
- [测试指南](../contributing/testing.md)
- [架构决策记录](../architecture/decisions/)

---

*文档生成时间：2026-04-24*
*ZeroClaw 版本：v0.7.3*
