# Bảo mật không gây cản trở

> ⚠️ **Trạng thái: Đề xuất / Lộ trình**
>
> Tài liệu này mô tả các hướng tiếp cận đề xuất và có thể bao gồm các lệnh hoặc cấu hình giả định.
> Để biết hành vi runtime hiện tại, xem [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md), và [troubleshooting.md](troubleshooting.md).

## Nguyên tắc cốt lõi
>
> **"Các tính năng bảo mật nên như túi khí — luôn hiện diện, bảo vệ, và vô hình cho đến khi cần."**

## Thiết kế: tự động phát hiện âm thầm

### 1. Không thêm bước wizard mới (giữ nguyên 9 bước, < 60 giây)

```rust
// Wizard không thay đổi
// Các tính năng bảo mật tự phát hiện ở nền

pub fn run_wizard() -> Result<Config> {
    // ... 9 bước hiện có, không thay đổi ...

    let config = Config {
        // ... các trường hiện có ...

        // MỚI: Bảo mật tự phát hiện (không hiển thị trong wizard)
        security: SecurityConfig::autodetect(),  // Âm thầm!
    };

    config.save().await?;
    Ok(config)
}
```

### 2. Logic tự phát hiện (chạy một lần khi khởi động lần đầu)

```rust
// src/security/detect.rs

impl SecurityConfig {
    /// Phát hiện sandbox khả dụng và bật tự động
    /// Trả về giá trị mặc định thông minh dựa trên nền tảng + công cụ có sẵn
    pub fn autodetect() -> Self {
        Self {
            // Sandbox: ưu tiên Landlock (native), rồi Firejail, rồi none
            sandbox: SandboxConfig::autodetect(),

            // Resource limits: luôn bật monitoring
            resources: ResourceLimits::default(),

            // Audit: bật mặc định, log vào config dir
            audit: AuditConfig::default(),

            // Mọi thứ khác: giá trị mặc định an toàn
            ..SecurityConfig::default()
        }
    }
}

impl SandboxConfig {
    pub fn autodetect() -> Self {
        #[cfg(target_os = "linux")]
        {
            // Ưu tiên Landlock (native, không phụ thuộc)
            if Self::probe_landlock() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Landlock,
                    ..Self::default()
                };
            }

            // Fallback: Firejail nếu đã cài
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
            // Thử Bubblewrap trên macOS
            if Self::probe_bubblewrap() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Bubblewrap,
                    ..Self::default()
                };
            }
        }

        // Fallback: tắt (nhưng vẫn có application-layer security)
        Self {
            enabled: false,
            backend: SandboxBackend::None,
            ..Self::default()
        }
    }

    #[cfg(target_os = "linux")]
    fn probe_landlock() -> bool {
        // Thử tạo Landlock ruleset tối thiểu
        // Nếu thành công, kernel hỗ trợ Landlock
        landlock::Ruleset::new()
            .set_access_fs(landlock::AccessFS::read_file)
            .add_path(Path::new("/tmp"), landlock::AccessFS::read_file)
            .map(|ruleset| ruleset.restrict_self().is_ok())
            .unwrap_or(false)
    }

    fn probe_firejail() -> bool {
        // Kiểm tra lệnh firejail có tồn tại không
        std::process::Command::new("firejail")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
```

### 3. Lần chạy đầu: ghi log âm thầm

```bash
$ zeroclaw agent -m "hello"

# Lần đầu: phát hiện âm thầm
[INFO] Detecting security features...
[INFO] ✓ Landlock sandbox enabled (kernel 6.2+)
[INFO] ✓ Memory monitoring active (512MB limit)
[INFO] ✓ Audit logging enabled (~/.config/zeroclaw/audit.log)

# Các lần sau: yên lặng
$ zeroclaw agent -m "hello"
[agent] Thinking...
```

### 4. File config: tất cả giá trị mặc định được ẩn

```toml
# ~/.config/zeroclaw/config.toml

# Các section này KHÔNG được ghi trừ khi người dùng tùy chỉnh
# [security.sandbox]
# enabled = true  # (mặc định, tự phát hiện)
# backend = "landlock"  # (mặc định, tự phát hiện)

# [security.resources]
# max_memory_mb = 512  # (mặc định)

# [security.audit]
# enabled = true  # (mặc định)
```

Chỉ khi người dùng thay đổi:
```toml
[security.sandbox]
enabled = false  # Người dùng tắt tường minh

[security.resources]
max_memory_mb = 1024  # Người dùng tăng giới hạn
```

### 5. Người dùng nâng cao: kiểm soát tường minh

```bash
# Kiểm tra trạng thái đang hoạt động
$ zeroclaw security --status
Security Status:
  ✓ Sandbox: Landlock (Linux kernel 6.2)
  ✓ Memory monitoring: 512MB limit
  ✓ Audit logging: ~/.config/zeroclaw/audit.log
  → 47 events logged today

# Tắt sandbox tường minh (ghi vào config)
$ zeroclaw config set security.sandbox.enabled false

# Bật backend cụ thể
$ zeroclaw config set security.sandbox.backend firejail

# Điều chỉnh giới hạn
$ zeroclaw config set security.resources.max_memory_mb 2048
```

### 6. Giảm cấp nhẹ nhàng

| Nền tảng | Tốt nhất có thể | Fallback | Tệ nhất |
|----------|---------------|----------|------------|
| **Linux 5.13+** | Landlock | None | Chỉ App-layer |
| **Linux (bất kỳ)** | Firejail | Landlock | Chỉ App-layer |
| **macOS** | Bubblewrap | None | Chỉ App-layer |
| **Windows** | None | - | Chỉ App-layer |

**App-layer security luôn hiện diện** — đây là allowlist/path blocking/injection protection hiện có, vốn đã toàn diện.

---

## Mở rộng config schema

```rust
// src/config/schema.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Cấu hình sandbox (tự phát hiện nếu không đặt)
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Giới hạn tài nguyên (áp dụng mặc định nếu không đặt)
    #[serde(default)]
    pub resources: ResourceLimits,

    /// Audit logging (bật mặc định)
    #[serde(default)]
    pub audit: AuditConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            sandbox: SandboxConfig::autodetect(),  // Phát hiện âm thầm!
            resources: ResourceLimits::default(),
            audit: AuditConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Bật sandboxing (mặc định: tự phát hiện)
    #[serde(default)]
    pub enabled: Option<bool>,  // None = tự phát hiện

    /// Sandbox backend (mặc định: tự phát hiện)
    #[serde(default)]
    pub backend: SandboxBackend,

    /// Tham số Firejail tùy chỉnh (tùy chọn)
    #[serde(default)]
    pub firejail_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    Auto,       // Tự phát hiện (mặc định)
    Landlock,   // Linux kernel LSM
    Firejail,   // User-space sandbox
    Bubblewrap, // User namespaces
    Docker,     // Container (nặng)
    None,       // Tắt
}

impl Default for SandboxBackend {
    fn default() -> Self {
        Self::Auto  // Luôn tự phát hiện mặc định
    }
}
```

---

## So sánh trải nghiệm người dùng

### Trước (hiện tại)

```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped
```

### Sau (với bảo mật không gây cản trở)

```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped | Landlock sandbox ✓
# ↑ Chỉ thêm một từ, tự phát hiện âm thầm!
```

### Người dùng nâng cao (kiểm soát tường minh)

```bash
$ zeroclaw onboard --security-level paranoid
[1/9] Workspace Setup...
...
✓ Security: Paranoid | Landlock + Firejail | Audit signed
```

---

## Tương thích ngược

| Tình huống | Hành vi |
|----------|----------|
| **Config hiện có** | Hoạt động không thay đổi, tính năng mới là opt-in |
| **Cài mới** | Tự phát hiện và bật bảo mật khả dụng |
| **Không có sandbox** | Fallback về app-layer (vẫn an toàn) |
| **Người dùng tắt** | Một flag config: `sandbox.enabled = false` |

---

## Tóm tắt

✅ **Không ảnh hưởng wizard** — giữ nguyên 9 bước, < 60 giây
✅ **Không thêm prompt** — tự phát hiện âm thầm
✅ **Không breaking change** — tương thích ngược
✅ **Có thể opt-out** — flag config tường minh
✅ **Hiển thị trạng thái** — `zeroclaw security --status`

Wizard vẫn là "thiết lập nhanh ứng dụng phổ quát" — bảo mật chỉ **lặng lẽ tốt hơn**.
