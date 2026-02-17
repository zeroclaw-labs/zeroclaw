# Frictionless Security: Zero Impact on Wizard

## Core Principle
> **"Security features should be like airbags — present, protective, and invisible until needed."**

## Design: Silent Auto-Detection

### 1. No New Wizard Steps (Stays 9 Steps, < 60 Seconds)

```rust
// Wizard remains UNCHANGED
// Security features auto-detect in background

pub fn run_wizard() -> Result<Config> {
    // ... existing 9 steps, no changes ...

    let config = Config {
        // ... existing fields ...

        // NEW: Auto-detected security (not shown in wizard)
        security: SecurityConfig::autodetect(),  // Silent!
    };

    config.save()?;
    Ok(config)
}
```

### 2. Auto-Detection Logic (Runs Once at First Start)

```rust
// src/security/detect.rs

impl SecurityConfig {
    /// Detect available sandboxing and enable automatically
    /// Returns smart defaults based on platform + available tools
    pub fn autodetect() -> Self {
        Self {
            // Sandbox: prefer Landlock (native), then Firejail, then none
            sandbox: SandboxConfig::autodetect(),

            // Resource limits: always enable monitoring
            resources: ResourceLimits::default(),

            // Audit: enable by default, log to config dir
            audit: AuditConfig::default(),

            // Everything else: safe defaults
            ..SecurityConfig::default()
        }
    }
}

impl SandboxConfig {
    pub fn autodetect() -> Self {
        #[cfg(target_os = "linux")]
        {
            // Prefer Landlock (native, no dependency)
            if Self::probe_landlock() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Landlock,
                    ..Self::default()
                };
            }

            // Fallback: Firejail if installed
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
            // Try Bubblewrap on macOS
            if Self::probe_bubblewrap() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Bubblewrap,
                    ..Self::default()
                };
            }
        }

        // Fallback: disabled (but still has application-layer security)
        Self {
            enabled: false,
            backend: SandboxBackend::None,
            ..Self::default()
        }
    }

    #[cfg(target_os = "linux")]
    fn probe_landlock() -> bool {
        // Try creating a minimal Landlock ruleset
        // If it works, kernel supports Landlock
        landlock::Ruleset::new()
            .set_access_fs(landlock::AccessFS::read_file)
            .add_path(Path::new("/tmp"), landlock::AccessFS::read_file)
            .map(|ruleset| ruleset.restrict_self().is_ok())
            .unwrap_or(false)
    }

    fn probe_firejail() -> bool {
        // Check if firejail command exists
        std::process::Command::new("firejail")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
```

### 3. First Run: Silent Logging

```bash
$ zeroclaw agent -m "hello"

# First time: silent detection
[INFO] Detecting security features...
[INFO] ✓ Landlock sandbox enabled (kernel 6.2+)
[INFO] ✓ Memory monitoring active (512MB limit)
[INFO] ✓ Audit logging enabled (~/.config/zeroclaw/audit.log)

# Subsequent runs: quiet
$ zeroclaw agent -m "hello"
[agent] Thinking...
```

### 4. Config File: All Defaults Hidden

```toml
# ~/.config/zeroclaw/config.toml

# These sections are NOT written unless user customizes
# [security.sandbox]
# enabled = true  # (default, auto-detected)
# backend = "landlock"  # (default, auto-detected)

# [security.resources]
# max_memory_mb = 512  # (default)

# [security.audit]
# enabled = true  # (default)
```

Only when user changes something:
```toml
[security.sandbox]
enabled = false  # User explicitly disabled

[security.resources]
max_memory_mb = 1024  # User increased limit
```

### 5. Advanced Users: Explicit Control

```bash
# Check what's active
$ zeroclaw security --status
Security Status:
  ✓ Sandbox: Landlock (Linux kernel 6.2)
  ✓ Memory monitoring: 512MB limit
  ✓ Audit logging: ~/.config/zeroclaw/audit.log
  → 47 events logged today

# Disable sandbox explicitly (writes to config)
$ zeroclaw config set security.sandbox.enabled false

# Enable specific backend
$ zeroclaw config set security.sandbox.backend firejail

# Adjust limits
$ zeroclaw config set security.resources.max_memory_mb 2048
```

### 6. Graceful Degradation

| Platform | Best Available | Fallback | Worst Case |
|----------|---------------|----------|------------|
| **Linux 5.13+** | Landlock | None | App-layer only |
| **Linux (any)** | Firejail | Landlock | App-layer only |
| **macOS** | Bubblewrap | None | App-layer only |
| **Windows** | None | - | App-layer only |

**App-layer security is always present** — this is the existing allowlist/path blocking/injection protection that's already comprehensive.

---

## Config Schema Extension

```rust
// src/config/schema.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Sandbox configuration (auto-detected if not set)
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Resource limits (defaults applied if not set)
    #[serde(default)]
    pub resources: ResourceLimits,

    /// Audit logging (enabled by default)
    #[serde(default)]
    pub audit: AuditConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            sandbox: SandboxConfig::autodetect(),  // Silent detection!
            resources: ResourceLimits::default(),
            audit: AuditConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Enable sandboxing (default: auto-detected)
    #[serde(default)]
    pub enabled: Option<bool>,  // None = auto-detect

    /// Sandbox backend (default: auto-detect)
    #[serde(default)]
    pub backend: SandboxBackend,

    /// Custom Firejail args (optional)
    #[serde(default)]
    pub firejail_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    Auto,       // Auto-detect (default)
    Landlock,   // Linux kernel LSM
    Firejail,   // User-space sandbox
    Bubblewrap, // User namespaces
    Docker,     // Container (heavy)
    None,       // Disabled
}

impl Default for SandboxBackend {
    fn default() -> Self {
        Self::Auto  // Always auto-detect by default
    }
}
```

---

## User Experience Comparison

### Before (Current)
```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped
```

### After (With Frictionless Security)
```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped | Landlock sandbox ✓
# ↑ Just one extra word, silent auto-detection!
```

### Advanced User (Explicit Control)
```bash
$ zeroclaw onboard --security-level paranoid
[1/9] Workspace Setup...
...
✓ Security: Paranoid | Landlock + Firejail | Audit signed
```

---

## Backward Compatibility

| Scenario | Behavior |
|----------|----------|
| **Existing config** | Works unchanged, new features opt-in |
| **New install** | Auto-detects and enables available security |
| **No sandbox available** | Falls back to app-layer (still secure) |
| **User disables** | One config flag: `sandbox.enabled = false` |

---

## Summary

✅ **Zero impact on wizard** — stays 9 steps, < 60 seconds
✅ **Zero new prompts** — silent auto-detection
✅ **Zero breaking changes** — backward compatible
✅ **Opt-out available** — explicit config flags
✅ **Status visibility** — `zeroclaw security --status`

The wizard remains "quick setup universal applications" — security is just **quietly better**.
