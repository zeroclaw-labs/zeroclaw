# 不可知安全：对可移植性零影响

> ⚠️ **状态：提案 / 路线图**
>
> 本文档描述提议的实现方法，可能包含假设的命令或配置。
> 如需了解当前运行时行为，请参见 [config-reference.zh-CN.md](../reference/api/config-reference.zh-CN.md)、[operations-runbook.zh-CN.md](../ops/operations-runbook.zh-CN.md) 和 [troubleshooting.zh-CN.md](../ops/troubleshooting.zh-CN.md)。

## 核心问题：安全功能是否会破坏...

1. ❓ 快速交叉编译构建？
2. ❓ 可插拔架构（任意替换）？
3. ❓ 硬件不可知性（ARM、x86、RISC-V）？
4. ❓ 小型硬件支持（<5MB RAM、10美元的板卡）？

**答案：全部不会** — 安全被设计为**可选特性标志**，带有**平台特定的条件编译**。

---

## 1. 构建速度：特性门控的安全

### Cargo.toml：特性背后的安全功能

```toml
[features]
default = [\"basic-security\"]

# 基础安全（始终开启，零开销）
basic-security = []

# 平台特定沙箱（按平台选择加入）
sandbox-landlock = []   # 仅 Linux
sandbox-firejail = []  # 仅 Linux
sandbox-bubblewrap = []# macOS/Linux
sandbox-docker = []    # 所有平台（重量级）

# 完整安全套件（用于生产构建）
security-full = [
    \"basic-security\",
    \"sandbox-landlock\",
    \"resource-monitoring\",
    \"audit-logging\",
]

# 资源与审计监控
resource-monitoring = []
audit-logging = []

# 开发构建（最快，无额外依赖）
dev = []
```

### 构建命令（选择你的配置文件）

```bash
# 超快速开发构建（无额外安全功能）
cargo build --profile dev

# 带基础安全的发布构建（默认）
cargo build --release
# → 包含：白名单、路径阻止、注入保护
# → 不包含：Landlock、Firejail、审计日志

# 带完整安全的生产构建
cargo build --release --features security-full
# → 包含所有功能

# 仅平台特定沙箱
cargo build --release --features sandbox-landlock  # Linux
cargo build --release --features sandbox-docker    # 所有平台
```

### 条件编译：禁用时零开销

```rust
// src/security/mod.rs

#[cfg(feature = \"sandbox-landlock\")]
mod landlock;
#[cfg(feature = \"sandbox-landlock\")]
pub use landlock::LandlockSandbox;

#[cfg(feature = \"sandbox-firejail\")]
mod firejail;
#[cfg(feature = \"sandbox-firejail\")]
pub use firejail::FirejailSandbox;

// 始终包含的基础安全（无特性标志）
pub mod policy;  // 白名单、路径阻止、注入保护
```

**结果：** 当特性被禁用时，代码甚至不会被编译 — **零二进制膨胀**。

---

## 2. 可插拔架构：安全也是 Trait

### 安全后端 Trait（像其他所有内容一样可交换）

```rust
// src/security/traits.rs

#[async_trait]
pub trait Sandbox: Send + Sync {
    /// 使用沙箱保护包装命令
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()>;

    /// 检查沙箱在此平台上是否可用
    fn is_available(&self) -> bool;

    /// 人类可读名称
    fn name(&self) -> &str;
}

// 无操作沙箱（始终可用）
pub struct NoopSandbox;

impl Sandbox for NoopSandbox {
    fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
        Ok(())  // 原封不动传递
    }

    fn is_available(&self) -> bool { true }
    fn name(&self) -> &str { \"none\" }
}
```

### 工厂模式：基于特性自动选择

```rust
// src/security/factory.rs

pub fn create_sandbox() -> Box<dyn Sandbox> {
    #[cfg(feature = \"sandbox-landlock\")]
    {
        if LandlockSandbox::is_available() {
            return Box::new(LandlockSandbox::new());
        }
    }

    #[cfg(feature = \"sandbox-firejail\")]
    {
        if FirejailSandbox::is_available() {
            return Box::new(FirejailSandbox::new());
        }
    }

    #[cfg(feature = \"sandbox-bubblewrap\")]
    {
        if BubblewrapSandbox::is_available() {
            return Box::new(BubblewrapSandbox::new());
        }
    }

    #[cfg(feature = \"sandbox-docker\")]
    {
        if DockerSandbox::is_available() {
            return Box::new(DockerSandbox::new());
        }
    }

    // 回退：始终可用
    Box::new(NoopSandbox)
}
```

**就像提供商、渠道和内存一样 — 安全也是可插拔的！**

---

## 3. 硬件不可知性：相同二进制，不同平台

### 跨平台行为矩阵

| 平台                    | 可构建 | 运行时行为                |
| ----------------------- | ------ | ------------------------- |
| **Linux ARM**（树莓派） | ✅ 是  | Landlock → 无（优雅降级） |
| **Linux x86_64**        | ✅ 是  | Landlock → Firejail → 无  |
| **macOS ARM**（M1/M2）  | ✅ 是  | Bubblewrap → 无           |
| **macOS x86_64**        | ✅ 是  | Bubblewrap → 无           |
| **Windows ARM**         | ✅ 是  | 无（应用层）              |
| **Windows x86_64**      | ✅ 是  | 无（应用层）              |
| **RISC-V Linux**        | ✅ 是  | Landlock → 无             |

### 工作原理：运行时检测

```rust
// src/security/detect.rs

impl SandboxingStrategy {
    /// 在运行时选择最佳可用沙箱
    pub fn detect() -> SandboxingStrategy {
        #[cfg(target_os = \"linux\")]
        {
            // 首先尝试 Landlock（内核特性检测）
            if Self::probe_landlock() {
                return SandboxingStrategy::Landlock;
            }

            // 尝试 Firejail（用户空间工具检测）
            if Self::probe_firejail() {
                return SandboxingStrategy::Firejail;
            }
        }

        #[cfg(target_os = \"macos\")]
        {
            if Self::probe_bubblewrap() {
                return SandboxingStrategy::Bubblewrap;
            }
        }

        // 始终可用的回退
        SandboxingStrategy::ApplicationLayer
    }
}
```

**相同二进制可在任何地方运行** — 它会根据可用功能自适应保护级别。

---

## 4. 小型硬件：内存影响分析

### 二进制大小影响（估算）

| 功能                | 代码大小 | RAM 开销       | 状态                |
| ------------------- | -------- | -------------- | ------------------- |
| **基础 JhedaiClaw** | 3.4MB    | <5MB           | ✅ 当前             |
| **+ Landlock**      | +50KB    | +100KB         | ✅ Linux 5.13+      |
| **+ Firejail 包装** | +20KB    | +0KB（外部）   | ✅ Linux + firejail |
| **+ 内存监控**      | +30KB    | +50KB          | ✅ 所有平台         |
| **+ 审计日志**      | +40KB    | +200KB（缓冲） | ✅ 所有平台         |
| **完整安全**        | +140KB   | +350KB         | ✅ 总计仍 <6MB      |

### 10美元硬件兼容性

| 硬件               | RAM   | JhedaiClaw（基础） | JhedaiClaw（完整安全） | 状态   |
| ------------------ | ----- | ------------------ | ---------------------- | ------ |
| **树莓派 Zero**    | 512MB | ✅ 2%              | ✅ 2.5%                | 可运行 |
| **Orange Pi Zero** | 512MB | ✅ 2%              | ✅ 2.5%                | 可运行 |
| **NanoPi NEO**     | 256MB | ✅ 4%              | ✅ 5%                  | 可运行 |
| **C.H.I.P.**       | 512MB | ✅ 2%              | ✅ 2.5%                | 可运行 |
| **Rock64**         | 1GB   | ✅ 1%              | ✅ 1.2%                | 可运行 |

**即使使用完整安全功能，JhedaiClaw 在 10美元板卡上的 RAM 占用也 <5%。**

---

## 5. 不可知交换：所有内容保持可插拔

### JhedaiClaw 的核心承诺：任意替换

```rust
// 提供商（已可插拔）
Box<dyn Provider>

// 渠道（已可插拔）
Box<dyn Channel>

// 内存（已可插拔）
Box<dyn MemoryBackend>

// 隧道（已可插拔）
Box<dyn Tunnel>

// 现在新增：安全（新增可插拔）
Box<dyn Sandbox>
Box<dyn Auditor>
Box<dyn ResourceMonitor>
```

### 通过配置交换安全后端

```toml
# 不使用沙箱（最快，仅应用层）
[security.sandbox]
backend = \"none\"

# 使用 Landlock（Linux 内核 LSM，原生）
[security.sandbox]
backend = \"landlock\"

# 使用 Firejail（用户空间，需要安装 firejail）
[security.sandbox]
backend = \"firejail\"

# 使用 Docker（最重，最隔离）
[security.sandbox]
backend = \"docker\"
```

**就像将 OpenAI 换成 Gemini，或者将 SQLite 换成 PostgreSQL 一样。**

---

## 6. 依赖影响：最小新依赖

### 当前依赖（供参考）

```
reqwest, tokio, serde, anyhow, uuid, chrono, rusqlite,
axum, tracing, opentelemetry, ...
```

### 安全功能依赖

| 功能           | 新依赖                        | 平台        |
| -------------- | ----------------------------- | ----------- |
| **Landlock**   | `landlock` crate（纯 Rust）   | 仅 Linux    |
| **Firejail**   | 无（外部二进制）              | 仅 Linux    |
| **Bubblewrap** | 无（外部二进制）              | macOS/Linux |
| **Docker**     | `bollard` crate（Docker API） | 所有平台    |
| **内存监控**   | 无（std::alloc）              | 所有平台    |
| **审计日志**   | 无（已有 hmac/sha2）          | 所有平台    |

**结果：** 大多数功能**不新增任何 Rust 依赖** — 它们要么：

1. 使用纯 Rust crate（landlock）
2. 包装外部二进制（Firejail、Bubblewrap）
3. 使用现有依赖（Cargo.toml 中已有 hmac、sha2）

---

## 总结：核心价值主张得以保留

| 价值主张               | 之前      | 之后（带安全）       | 状态    |
| ---------------------- | --------- | -------------------- | ------- |
| **<5MB RAM**           | ✅ <5MB   | ✅ <6MB（最坏情况）  | ✅ 保留 |
| **<10ms 启动**         | ✅ <10ms  | ✅ <15ms（检测）     | ✅ 保留 |
| **3.4MB 二进制**       | ✅ 3.4MB  | ✅ 3.5MB（所有功能） | ✅ 保留 |
| **ARM + x86 + RISC-V** | ✅ 全部   | ✅ 全部              | ✅ 保留 |
| **10美元硬件**         | ✅ 可运行 | ✅ 可运行            | ✅ 保留 |
| **所有内容可插拔**     | ✅ 是     | ✅ 是（安全也如此）  | ✅ 增强 |
| **跨平台**             | ✅ 是     | ✅ 是                | ✅ 保留 |

---

## 关键：特性标志 + 条件编译

```bash
# 开发人员构建（最快，无额外功能）
cargo build --profile dev

# 标准发布（你当前的构建）
cargo build --release

# 带完整安全的生产构建
cargo build --release --features security-full

# 针对特定硬件
cargo build --release --target aarch64-unknown-linux-gnu  # 树莓派
cargo build --release --target riscv64gc-unknown-linux-gnu # RISC-V
cargo build --release --target armv7-unknown-linux-gnueabihf  # ARMv7
```

**每个目标、每个平台、每个用例 — 仍然快速、仍然小巧、仍然不可知。**
