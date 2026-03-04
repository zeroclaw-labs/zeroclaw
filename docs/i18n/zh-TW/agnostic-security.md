# 平台無關安全性：對可攜性零影響（繁體中文）

> ⚠️ **狀態：提案 / 規劃路線**
>
> 本文件描述提案中的方法，可能包含假設性的指令或設定。
> 有關目前的執行期行為，請參閱 [config-reference.md](../../config-reference.md)、[operations-runbook.md](../../operations-runbook.md) 及 [troubleshooting.md](../../troubleshooting.md)。

## 核心問題：安全功能會破壞以下特性嗎...

1. ❓ 快速交叉編譯建置？
2. ❓ 可插拔架構（任意替換元件）？
3. ❓ 硬體無關性（ARM、x86、RISC-V）？
4. ❓ 小型硬體支援（<5MB 記憶體、$10 開發板）？

**答案：以上皆否** — 安全性的設計採用**選用式功能旗標（feature flags）**搭配**平台專屬條件式編譯**。

---

## 1. 建置速度：功能閘控的安全性

### Cargo.toml：以 Feature 封裝安全功能

```toml
[features]
default = ["basic-security"]

# 基礎安全（始終啟用，零額外開銷）
basic-security = []

# 平台專屬沙箱（按平台選擇性啟用）
sandbox-landlock = []   # 僅限 Linux
sandbox-firejail = []  # 僅限 Linux
sandbox-bubblewrap = []# macOS/Linux
sandbox-docker = []    # 全平台（較重）

# 完整安全套件（用於正式環境建置）
security-full = [
    "basic-security",
    "sandbox-landlock",
    "resource-monitoring",
    "audit-logging",
]

# 資源與稽核監控
resource-monitoring = []
audit-logging = []

# 開發建置（最快速，無額外相依）
dev = []
```

### 建置指令（依需求選擇設定檔）

```bash
# 超快速開發建置（不含安全性額外功能）
cargo build --profile dev

# 含基礎安全的 Release 建置（預設）
cargo build --release
# → 包含：允許清單、路徑封鎖、注入防護
# → 不含：Landlock、Firejail、稽核日誌

# 含完整安全性的正式建置
cargo build --release --features security-full
# → 包含：所有功能

# 僅啟用特定平台沙箱
cargo build --release --features sandbox-landlock  # Linux
cargo build --release --features sandbox-docker    # 全平台
```

### 條件式編譯：停用時零額外開銷

```rust
// src/security/mod.rs

#[cfg(feature = "sandbox-landlock")]
mod landlock;
#[cfg(feature = "sandbox-landlock")]
pub use landlock::LandlockSandbox;

#[cfg(feature = "sandbox-firejail")]
mod firejail;
#[cfg(feature = "sandbox-firejail")]
pub use firejail::FirejailSandbox;

// 始終包含的基礎安全（無需功能旗標）
pub mod policy;  // 允許清單、路徑封鎖、注入防護
```

**結果**：當功能被停用時，對應程式碼根本不會被編譯 — **二進位檔零膨脹**。

---

## 2. 可插拔架構：安全性也是一個 Trait

### 安全後端 Trait（如同其他元件一樣可替換）

```rust
// src/security/traits.rs

#[async_trait]
pub trait Sandbox: Send + Sync {
    /// 以沙箱保護包裝指令
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()>;

    /// 檢查沙箱在此平台上是否可用
    fn is_available(&self) -> bool;

    /// 人類可讀的名稱
    fn name(&self) -> &str;
}

// 空操作沙箱（始終可用）
pub struct NoopSandbox;

impl Sandbox for NoopSandbox {
    fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
        Ok(())  // 直接通過，不做變更
    }

    fn is_available(&self) -> bool { true }
    fn name(&self) -> &str { "none" }
}
```

### 工廠模式：依功能自動選擇

```rust
// src/security/factory.rs

pub fn create_sandbox() -> Box<dyn Sandbox> {
    #[cfg(feature = "sandbox-landlock")]
    {
        if LandlockSandbox::is_available() {
            return Box::new(LandlockSandbox::new());
        }
    }

    #[cfg(feature = "sandbox-firejail")]
    {
        if FirejailSandbox::is_available() {
            return Box::new(FirejailSandbox::new());
        }
    }

    #[cfg(feature = "sandbox-bubblewrap")]
    {
        if BubblewrapSandbox::is_available() {
            return Box::new(BubblewrapSandbox::new());
        }
    }

    #[cfg(feature = "sandbox-docker")]
    {
        if DockerSandbox::is_available() {
            return Box::new(DockerSandbox::new());
        }
    }

    // 備援：始終可用
    Box::new(NoopSandbox)
}
```

**就像 Provider、Channel 和 Memory 一樣 — 安全性也是可插拔的！**

---

## 3. 硬體無關性：同一個二進位檔，不同平台

### 跨平台行為矩陣

| 平台 | 可建置 | 執行期行為 |
|------|--------|-----------|
| **Linux ARM**（Raspberry Pi） | ✅ 是 | Landlock → None（優雅降級） |
| **Linux x86_64** | ✅ 是 | Landlock → Firejail → None |
| **macOS ARM**（M1/M2） | ✅ 是 | Bubblewrap → None |
| **macOS x86_64** | ✅ 是 | Bubblewrap → None |
| **Windows ARM** | ✅ 是 | None（應用層防護） |
| **Windows x86_64** | ✅ 是 | None（應用層防護） |
| **RISC-V Linux** | ✅ 是 | Landlock → None |

### 運作方式：執行期偵測

```rust
// src/security/detect.rs

impl SandboxingStrategy {
    /// 在執行期選擇最佳可用的沙箱
    pub fn detect() -> SandboxingStrategy {
        #[cfg(target_os = "linux")]
        {
            // 優先嘗試 Landlock（核心功能偵測）
            if Self::probe_landlock() {
                return SandboxingStrategy::Landlock;
            }

            // 嘗試 Firejail（使用者空間工具偵測）
            if Self::probe_firejail() {
                return SandboxingStrategy::Firejail;
            }
        }

        #[cfg(target_os = "macos")]
        {
            if Self::probe_bubblewrap() {
                return SandboxingStrategy::Bubblewrap;
            }
        }

        // 始終可用的備援方案
        SandboxingStrategy::ApplicationLayer
    }
}
```

**同一個二進位檔在所有平台上運行** — 只是根據可用資源調整保護層級。

---

## 4. 小型硬體：記憶體影響分析

### 二進位檔大小影響（估計值）

| 功能 | 程式碼大小 | 記憶體額外開銷 | 狀態 |
|------|-----------|--------------|------|
| **基礎 ZeroClaw** | 3.4MB | <5MB | ✅ 目前 |
| **+ Landlock** | +50KB | +100KB | ✅ Linux 5.13+ |
| **+ Firejail 包裝器** | +20KB | +0KB（外部工具） | ✅ Linux + firejail |
| **+ 記憶體監控** | +30KB | +50KB | ✅ 全平台 |
| **+ 稽核日誌** | +40KB | +200KB（含緩衝） | ✅ 全平台 |
| **完整安全性** | +140KB | +350KB | ✅ 仍低於 6MB |

### $10 硬體相容性

| 硬體 | 記憶體 | ZeroClaw（基礎） | ZeroClaw（完整安全性） | 狀態 |
|------|--------|-----------------|----------------------|------|
| **Raspberry Pi Zero** | 512MB | ✅ 2% | ✅ 2.5% | 可運行 |
| **Orange Pi Zero** | 512MB | ✅ 2% | ✅ 2.5% | 可運行 |
| **NanoPi NEO** | 256MB | ✅ 4% | ✅ 5% | 可運行 |
| **C.H.I.P.** | 512MB | ✅ 2% | ✅ 2.5% | 可運行 |
| **Rock64** | 1GB | ✅ 1% | ✅ 1.2% | 可運行 |

**即使啟用完整安全性，ZeroClaw 在 $10 開發板上的記憶體使用量仍低於 5%。**

---

## 5. 無關性替換：一切維持可插拔

### ZeroClaw 的核心承諾：任意替換

```rust
// Provider（已可插拔）
Box<dyn Provider>

// Channel（已可插拔）
Box<dyn Channel>

// Memory（已可插拔）
Box<dyn MemoryBackend>

// Tunnel（已可插拔）
Box<dyn Tunnel>

// 現在也包括：安全性（新增可插拔）
Box<dyn Sandbox>
Box<dyn Auditor>
Box<dyn ResourceMonitor>
```

### 透過設定檔替換安全後端

```toml
# 不使用沙箱（最快速，僅應用層防護）
[security.sandbox]
backend = "none"

# 使用 Landlock（Linux 核心 LSM，原生支援）
[security.sandbox]
backend = "landlock"

# 使用 Firejail（使用者空間，需安裝 firejail）
[security.sandbox]
backend = "firejail"

# 使用 Docker（最重量級，最完整隔離）
[security.sandbox]
backend = "docker"
```

**就像將 OpenAI 換成 Gemini，或將 SQLite 換成 PostgreSQL 一樣簡單。**

---

## 6. 相依性影響：新增相依極少

### 目前的相依套件（供參考）

```
reqwest, tokio, serde, anyhow, uuid, chrono, rusqlite,
axum, tracing, opentelemetry, ...
```

### 安全功能相依套件

| 功能 | 新增相依 | 平台 |
|------|---------|------|
| **Landlock** | `landlock` crate（純 Rust） | 僅限 Linux |
| **Firejail** | 無（外部二進位檔） | 僅限 Linux |
| **Bubblewrap** | 無（外部二進位檔） | macOS/Linux |
| **Docker** | `bollard` crate（Docker API） | 全平台 |
| **記憶體監控** | 無（std::alloc） | 全平台 |
| **稽核日誌** | 無（已有 hmac/sha2） | 全平台 |

**結果**：大多數功能**不需新增任何 Rust 相依套件** — 它們要麼：
1. 使用純 Rust crate（Landlock）
2. 包裝外部二進位檔（Firejail、Bubblewrap）
3. 使用既有相依（hmac、sha2 已在 Cargo.toml 中）

---

## 總結：核心價值主張完全保留

| 價值主張 | 變更前 | 變更後（含安全性） | 狀態 |
|---------|--------|-------------------|------|
| **<5MB 記憶體** | ✅ <5MB | ✅ <6MB（最差情況） | ✅ 保留 |
| **<10ms 啟動** | ✅ <10ms | ✅ <15ms（含偵測） | ✅ 保留 |
| **3.4MB 二進位檔** | ✅ 3.4MB | ✅ 3.5MB（啟用所有功能） | ✅ 保留 |
| **ARM + x86 + RISC-V** | ✅ 全部 | ✅ 全部 | ✅ 保留 |
| **$10 硬體** | ✅ 可運行 | ✅ 可運行 | ✅ 保留 |
| **一切可插拔** | ✅ 是 | ✅ 是（安全性也是） | ✅ 增強 |
| **跨平台** | ✅ 是 | ✅ 是 | ✅ 保留 |

---

## 關鍵：功能旗標 + 條件式編譯

```bash
# 開發建置（最快速，無額外功能）
cargo build --profile dev

# 標準 Release（你目前的建置方式）
cargo build --release

# 正式環境含完整安全性
cargo build --release --features security-full

# 針對特定硬體目標
cargo build --release --target aarch64-unknown-linux-gnu  # Raspberry Pi
cargo build --release --target riscv64gc-unknown-linux-gnu # RISC-V
cargo build --release --target armv7-unknown-linux-gnueabihf  # ARMv7
```

**所有目標、所有平台、所有使用情境 — 依然快速、依然輕巧、依然平台無關。**
