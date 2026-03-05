# 無摩擦安全性：對設定精靈零影響（繁體中文）

> ⚠️ **狀態：提案 / 規劃路線**
>
> 本文件描述提案中的方法，可能包含假設性的指令或設定。
> 有關目前的執行期行為，請參閱 [config-reference.md](config-reference.md)、[operations-runbook.md](operations-runbook.md) 及 [troubleshooting.md](troubleshooting.md)。

## 核心原則

> **「安全功能應該像安全氣囊一樣 — 存在、具保護力，而且在需要之前完全隱形。」**

## 設計：靜默式自動偵測

### 1. 不新增精靈步驟（維持 9 步驟，< 60 秒）

```rust
// 設定精靈保持不變
// 安全功能在背景自動偵測

pub fn run_wizard() -> Result<Config> {
    // ... 既有的 9 個步驟，不做任何變更 ...

    let config = Config {
        // ... 既有欄位 ...

        // 新增：自動偵測的安全性（不顯示在精靈中）
        security: SecurityConfig::autodetect(),  // 靜默執行！
    };

    config.save().await?;
    Ok(config)
}
```

### 2. 自動偵測邏輯（首次啟動時執行一次）

```rust
// src/security/detect.rs

impl SecurityConfig {
    /// 偵測可用的沙箱功能並自動啟用
    /// 根據平台與可用工具回傳智慧預設值
    pub fn autodetect() -> Self {
        Self {
            // 沙箱：優先 Landlock（原生），接著 Firejail，最後不啟用
            sandbox: SandboxConfig::autodetect(),

            // 資源限制：始終啟用監控
            resources: ResourceLimits::default(),

            // 稽核：預設啟用，記錄至設定目錄
            audit: AuditConfig::default(),

            // 其餘項目：安全預設值
            ..SecurityConfig::default()
        }
    }
}

impl SandboxConfig {
    pub fn autodetect() -> Self {
        #[cfg(target_os = "linux")]
        {
            // 優先使用 Landlock（原生，無額外相依）
            if Self::probe_landlock() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Landlock,
                    ..Self::default()
                };
            }

            // 備援：若已安裝 Firejail 則使用
            if Self::probe_firejail() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Firejail,
                    ..Self::default()
                };
            }
        }

        #[cfg(target_os = "macos")]
        {
            // 在 macOS 上嘗試 Bubblewrap
            if Self::probe_bubblewrap() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Bubblewrap,
                    ..Self::default()
                };
            }
        }

        // 備援：停用（但仍有應用層安全防護）
        Self {
            enabled: false,
            backend: SandboxBackend::None,
            ..Self::default()
        }
    }

    #[cfg(target_os = "linux")]
    fn probe_landlock() -> bool {
        // 嘗試建立最小的 Landlock 規則集
        // 若成功，代表核心支援 Landlock
        landlock::Ruleset::new()
            .set_access_fs(landlock::AccessFS::read_file)
            .add_path(Path::new("/tmp"), landlock::AccessFS::read_file)
            .map(|ruleset| ruleset.restrict_self().is_ok())
            .unwrap_or(false)
    }

    fn probe_firejail() -> bool {
        // 檢查 firejail 指令是否存在
        std::process::Command::new("firejail")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
```

### 3. 首次執行：靜默記錄

```bash
$ zeroclaw agent -m "hello"

# 首次執行：靜默偵測
[INFO] Detecting security features...
[INFO] ✓ Landlock sandbox enabled (kernel 6.2+)
[INFO] ✓ Memory monitoring active (512MB limit)
[INFO] ✓ Audit logging enabled (~/.config/zeroclaw/audit.log)

# 後續執行：安靜模式
$ zeroclaw agent -m "hello"
[agent] Thinking...
```

### 4. 設定檔：所有預設值隱藏

```toml
# ~/.config/zeroclaw/config.toml

# 除非使用者自訂，否則這些區段不會被寫入
# [security.sandbox]
# enabled = true  # （預設值，自動偵測）
# backend = "landlock"  # （預設值，自動偵測）

# [security.resources]
# max_memory_mb = 512  # （預設值）

# [security.audit]
# enabled = true  # （預設值）
```

只有當使用者主動變更時才會寫入：
```toml
[security.sandbox]
enabled = false  # 使用者明確停用

[security.resources]
max_memory_mb = 1024  # 使用者調高上限
```

### 5. 進階使用者：明確控制

```bash
# 檢查目前啟用的功能
$ zeroclaw security --status
Security Status:
  ✓ Sandbox: Landlock (Linux kernel 6.2)
  ✓ Memory monitoring: 512MB limit
  ✓ Audit logging: ~/.config/zeroclaw/audit.log
  → 47 events logged today

# 明確停用沙箱（寫入設定檔）
$ zeroclaw config set security.sandbox.enabled false

# 啟用特定後端
$ zeroclaw config set security.sandbox.backend firejail

# 調整限制值
$ zeroclaw config set security.resources.max_memory_mb 2048
```

### 6. 優雅降級

| 平台 | 最佳方案 | 備援方案 | 最差情況 |
|------|---------|---------|---------|
| **Linux 5.13+** | Landlock | None | 僅應用層防護 |
| **Linux（任意版本）** | Firejail | Landlock | 僅應用層防護 |
| **macOS** | Bubblewrap | None | 僅應用層防護 |
| **Windows** | None | - | 僅應用層防護 |

**應用層安全防護始終存在** — 這就是既有的允許清單 / 路徑封鎖 / 注入防護，已經相當全面。

---

## 設定架構擴充

```rust
// src/config/schema.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// 沙箱設定（未設定時自動偵測）
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// 資源限制（未設定時套用預設值）
    #[serde(default)]
    pub resources: ResourceLimits,

    /// 稽核日誌（預設啟用）
    #[serde(default)]
    pub audit: AuditConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            sandbox: SandboxConfig::autodetect(),  // 靜默偵測！
            resources: ResourceLimits::default(),
            audit: AuditConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// 是否啟用沙箱（預設：自動偵測）
    #[serde(default)]
    pub enabled: Option<bool>,  // None = 自動偵測

    /// 沙箱後端（預設：自動偵測）
    #[serde(default)]
    pub backend: SandboxBackend,

    /// 自訂 Firejail 參數（選用）
    #[serde(default)]
    pub firejail_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    Auto,       // 自動偵測（預設）
    Landlock,   // Linux 核心 LSM
    Firejail,   // 使用者空間沙箱
    Bubblewrap, // 使用者命名空間
    Docker,     // 容器（較重）
    None,       // 停用
}

impl Default for SandboxBackend {
    fn default() -> Self {
        Self::Auto  // 始終預設自動偵測
    }
}
```

---

## 使用者體驗對照

### 變更前（目前）

```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped
```

### 變更後（含無摩擦安全性）

```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped | Landlock sandbox ✓
# ↑ 只多一個字，靜默自動偵測！
```

### 進階使用者（明確控制）

```bash
$ zeroclaw onboard --security-level paranoid
[1/9] Workspace Setup...
...
✓ Security: Paranoid | Landlock + Firejail | Audit signed
```

---

## 向下相容性

| 情境 | 行為 |
|------|------|
| **既有設定檔** | 照常運作，新功能為選擇性啟用 |
| **全新安裝** | 自動偵測並啟用可用的安全功能 |
| **無沙箱可用** | 降級為應用層防護（仍具安全性） |
| **使用者停用** | 單一設定旗標：`sandbox.enabled = false` |

---

## 總結

✅ **對設定精靈零影響** — 維持 9 步驟，< 60 秒
✅ **不新增提示** — 靜默式自動偵測
✅ **不產生破壞性變更** — 向下相容
✅ **可選擇退出** — 明確的設定旗標
✅ **狀態可見性** — `zeroclaw security --status`

設定精靈維持「快速設定萬用程式」的定位 — 安全性只是**靜靜變得更好了**。
