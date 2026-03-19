# 无摩擦安全：对安装向导零影响

> ⚠️ **状态：提案 / 路线图**
>
> 本文档描述提议的实现方法，可能包含假设的命令或配置。
> 如需了解当前运行时行为，请参见 [config-reference.zh-CN.md](../reference/api/config-reference.zh-CN.md)、[operations-runbook.zh-CN.md](../ops/operations-runbook.zh-CN.md) 和 [troubleshooting.zh-CN.md](../ops/troubleshooting.zh-CN.md)。

## 核心原则

> **"安全功能应该像安全气囊 — 存在、有保护作用，且在需要之前不可见。"**

## 设计：静默自动检测

### 1. 无新的向导步骤（保持 9 步，< 60 秒）

```rust
// 向导保持不变
// 安全功能在后台自动检测

pub fn run_wizard() -> Result<Config> {
    // ... 现有 9 步，无更改 ...

    let config = Config {
        // ... 现有字段 ...

        // 新增：自动检测的安全（不在向导中显示）
        security: SecurityConfig::autodetect(),  // 静默！
    };

    config.save().await?;
    Ok(config)
}
```

### 2. 自动检测逻辑（首次启动时运行一次）

```rust
// src/security/detect.rs

impl SecurityConfig {
    /// 检测可用的沙箱并自动启用
    /// 基于平台 + 可用工具返回智能默认值
    pub fn autodetect() -> Self {
        Self {
            // 沙箱：优先 Landlock（原生），然后 Firejail，然后无
            sandbox: SandboxConfig::autodetect(),

            // 资源限制：始终启用监控
            resources: ResourceLimits::default(),

            // 审计：默认启用，记录到配置目录
            audit: AuditConfig::default(),

            // 其他所有项：安全默认值
            ..SecurityConfig::default()
        }
    }
}

impl SandboxConfig {
    pub fn autodetect() -> Self {
        #[cfg(target_os = \"linux\")]
        {
            // 优先 Landlock（原生，无依赖）
            if Self::probe_landlock() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Landlock,
                    ..Self::default()
                };
            }

            // 回退：如果安装了 Firejail 则使用
            if Self::probe_firejail() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Firejail,
                    ..Self::default()
                };
            }
        }

        #[cfg(target_os = \"macos\")]
        {
            // 在 macOS 上尝试 Bubblewrap
            if Self::probe_bubblewrap() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Bubblewrap,
                    ..Self::default()
                };
            }
        }

        // 回退：禁用（但仍有应用层安全）
        Self {
            enabled: false,
            backend: SandboxBackend::None,
            ..Self::default()
        }
    }

    #[cfg(target_os = \"linux\")]
    fn probe_landlock() -> bool {
        // 尝试创建最小 Landlock 规则集
        // 如果成功，内核支持 Landlock
        landlock::Ruleset::new()
            .set_access_fs(landlock::AccessFS::read_file)
            .add_path(Path::new(\"/tmp\"), landlock::AccessFS::read_file)
            .map(|ruleset| ruleset.restrict_self().is_ok())
            .unwrap_or(false)
    }

    fn probe_firejail() -> bool {
        // 检查 firejail 命令是否存在
        std::process::Command::new(\"firejail\")
            .arg(\"--version\")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
```

### 3. 首次运行：静默日志

```bash
$ jhedaiclaw agent -m \"hello\"

# 首次运行：静默检测
[INFO] Detecting security features...
[INFO] ✓ Landlock sandbox enabled (kernel 6.2+)
[INFO] ✓ Memory monitoring active (512MB limit)
[INFO] ✓ Audit logging enabled (~/.config/jhedaiclaw/audit.log)

# 后续运行：安静
$ jhedaiclaw agent -m \"hello\"
[agent] Thinking...
```

### 4. 配置文件：所有默认值隐藏

```toml
# ~/.config/jhedaiclaw/config.toml

# 这些部分不会被写入，除非用户自定义
# [security.sandbox]
# enabled = true  # （默认，自动检测）
# backend = \"landlock\"  # （默认，自动检测）

# [security.resources]
# max_memory_mb = 512  # （默认）

# [security.audit]
# enabled = true  # （默认）
```

仅当用户更改某些内容时：

```toml
[security.sandbox]
enabled = false  # 用户显式禁用

[security.resources]
max_memory_mb = 1024  # 用户提高了限制
```

### 5. 高级用户：显式控制

```bash
# 检查哪些功能处于活动状态
$ jhedaiclaw security --status
Security Status:
  ✓ Sandbox: Landlock (Linux kernel 6.2)
  ✓ Memory monitoring: 512MB limit
  ✓ Audit logging: ~/.config/jhedaiclaw/audit.log
  → 今日已记录 47 个事件

# 显式禁用沙箱（写入配置）
$ jhedaiclaw config set security.sandbox.enabled false

# 启用特定后端
$ jhedaiclaw config set security.sandbox.backend firejail

# 调整限制
$ jhedaiclaw config set security.resources.max_memory_mb 2048
```

### 6. 优雅降级

| 平台                  | 最佳可用   | 回退     | 最坏情况 |
| --------------------- | ---------- | -------- | -------- |
| **Linux 5.13+**       | Landlock   | 无       | 仅应用层 |
| **Linux（任意版本）** | Firejail   | Landlock | 仅应用层 |
| **macOS**             | Bubblewrap | 无       | 仅应用层 |
| **Windows**           | 无         | -        | 仅应用层 |

**应用层安全始终存在** — 这是现有的白名单/路径阻止/注入保护，已经很全面。

---

## 配置模式扩展

```rust
// src/config/schema.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// 沙箱配置（未设置则自动检测）
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// 资源限制（未设置则应用默认值）
    #[serde(default)]
    pub resources: ResourceLimits,

    /// 审计日志（默认启用）
    #[serde(default)]
    pub audit: AuditConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            sandbox: SandboxConfig::autodetect(),  // 静默检测！
            resources: ResourceLimits::default(),
            audit: AuditConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// 启用沙箱（默认：自动检测）
    #[serde(default)]
    pub enabled: Option<bool>,  // None = 自动检测

    /// 沙箱后端（默认：自动检测）
    #[serde(default)]
    pub backend: SandboxBackend,

    /// 自定义 Firejail 参数（可选）
    #[serde(default)]
    pub firejail_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = \"lowercase\")]
pub enum SandboxBackend {
    Auto,       // 自动检测（默认）
    Landlock,   // Linux 内核 LSM
    Firejail,   // 用户空间沙箱
    Bubblewrap, // 用户命名空间
    Docker,     // 容器（重量级）
    None,       // 禁用
}

impl Default for SandboxBackend {
    fn default() -> Self {
        Self::Auto  // 默认始终自动检测
    }
}
```

---

## 用户体验对比

### 之前（当前）

```bash
$ jhedaiclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped
```

### 之后（带无摩擦安全）

```bash
$ jhedaiclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped | Landlock sandbox ✓
# ↑ 仅多了一个词，静默自动检测！
```

---

## 向后兼容性

| 场景           | 行为                                    |
| -------------- | --------------------------------------- |
| **现有配置**   | 工作不变，新功能选择加入                |
| **新安装**     | 自动检测并启用可用的安全功能            |
| **无可用沙箱** | 回退到应用层（仍然安全）                |
| **用户禁用**   | 一个配置标志：`sandbox.enabled = false` |

---

## 总结

✅ **对向导零影响** — 保持 9 步，< 60 秒
✅ **无新提示** — 静默自动检测
✅ **无破坏性变更** — 向后兼容
✅ **可选择退出** — 显式配置标志
✅ **状态可见性** — `jhedaiclaw security --status`

向导仍然是「通用应用快速安装」 — 安全只是**默默地更好了**。
