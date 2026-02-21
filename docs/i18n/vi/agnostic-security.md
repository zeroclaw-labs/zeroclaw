# Bảo mật không phụ thuộc nền tảng

> ⚠️ **Trạng thái: Đề xuất / Lộ trình**
>
> Tài liệu này mô tả các hướng tiếp cận đề xuất và có thể bao gồm các lệnh hoặc cấu hình giả định.
> Để biết hành vi runtime hiện tại, xem [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md), và [troubleshooting.md](troubleshooting.md).

## Câu hỏi cốt lõi: liệu các tính năng bảo mật có làm hỏng

1. ❓ Quá trình cross-compilation nhanh?
2. ❓ Kiến trúc pluggable (hoán đổi bất kỳ thành phần nào)?
3. ❓ Tính agnostic phần cứng (ARM, x86, RISC-V)?
4. ❓ Hỗ trợ phần cứng nhỏ (<5MB RAM, board $10)?

**Câu trả lời: KHÔNG với tất cả** — Bảo mật được thiết kế dưới dạng **feature flags tùy chọn** với **conditional compilation theo từng nền tảng**.

---

## 1. Tốc độ build: bảo mật ẩn sau feature flag

### Cargo.toml: các tính năng bảo mật đặt sau features

```toml
[features]
default = ["basic-security"]

# Basic security (luôn bật, không tốn overhead)
basic-security = []

# Platform-specific sandboxing (opt-in theo từng nền tảng)
sandbox-landlock = []   # Chỉ Linux
sandbox-firejail = []  # Chỉ Linux
sandbox-bubblewrap = []# macOS/Linux
sandbox-docker = []    # Tất cả nền tảng (nặng)

# Bộ bảo mật đầy đủ (dành cho production build)
security-full = [
    "basic-security",
    "sandbox-landlock",
    "resource-monitoring",
    "audit-logging",
]

# Resource & audit monitoring
resource-monitoring = []
audit-logging = []

# Development build (nhanh nhất, không phụ thuộc thêm)
dev = []
```

### Lệnh build (chọn profile phù hợp)

```bash
# Dev build cực nhanh (không có extras bảo mật)
cargo build --profile dev

# Release build với basic security (mặc định)
cargo build --release
# → Bao gồm: allowlist, path blocking, injection protection
# → Không bao gồm: Landlock, Firejail, audit logging

# Production build với full security
cargo build --release --features security-full
# → Bao gồm: Tất cả

# Chỉ sandbox theo nền tảng cụ thể
cargo build --release --features sandbox-landlock  # Linux
cargo build --release --features sandbox-docker    # Tất cả nền tảng
```

### Conditional compilation: không overhead khi tắt

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

// Basic security luôn được include (không cần feature flag)
pub mod policy;  // allowlist, path blocking, injection protection
```

**Kết quả**: Khi các feature bị tắt, code thậm chí không được biên dịch — **binary hoàn toàn không bị phình to**.

---

## 2. Kiến trúc pluggable: bảo mật cũng là một trait

### Security backend trait (hoán đổi như mọi thứ khác)

```rust
// src/security/traits.rs

#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Bọc lệnh với lớp bảo vệ sandbox
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()>;

    /// Kiểm tra sandbox có khả dụng trên nền tảng này không
    fn is_available(&self) -> bool;

    /// Tên dễ đọc
    fn name(&self) -> &str;
}

// No-op sandbox (luôn khả dụng)
pub struct NoopSandbox;

impl Sandbox for NoopSandbox {
    fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
        Ok(())  // Pass-through, không thay đổi
    }

    fn is_available(&self) -> bool { true }
    fn name(&self) -> &str { "none" }
}
```

### Factory pattern: tự động chọn dựa trên features

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

    // Fallback: luôn khả dụng
    Box::new(NoopSandbox)
}
```

**Giống như providers, channels và memory — bảo mật cũng là pluggable!**

---

## 3. Agnostic phần cứng: cùng binary, nhiều nền tảng

### Ma trận hành vi đa nền tảng

| Nền tảng | Build trên | Hành vi runtime |
|----------|-----------|------------------|
| **Linux ARM** (Raspberry Pi) | ✅ Có | Landlock → None (graceful) |
| **Linux x86_64** | ✅ Có | Landlock → Firejail → None |
| **macOS ARM** (M1/M2) | ✅ Có | Bubblewrap → None |
| **macOS x86_64** | ✅ Có | Bubblewrap → None |
| **Windows ARM** | ✅ Có | None (app-layer) |
| **Windows x86_64** | ✅ Có | None (app-layer) |
| **RISC-V Linux** | ✅ Có | Landlock → None |

### Cơ chế hoạt động: phát hiện tại runtime

```rust
// src/security/detect.rs

impl SandboxingStrategy {
    /// Chọn sandbox tốt nhất có sẵn TẠI RUNTIME
    pub fn detect() -> SandboxingStrategy {
        #[cfg(target_os = "linux")]
        {
            // Thử Landlock trước (phát hiện tính năng kernel)
            if Self::probe_landlock() {
                return SandboxingStrategy::Landlock;
            }

            // Thử Firejail (phát hiện công cụ user-space)
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

        // Fallback luôn khả dụng
        SandboxingStrategy::ApplicationLayer
    }
}
```

**Cùng một binary chạy ở khắp nơi** — chỉ tự điều chỉnh mức độ bảo vệ dựa trên những gì có sẵn.

---

## 4. Phần cứng nhỏ: phân tích tác động bộ nhớ

### Tác động kích thước binary (ước tính)

| Tính năng | Kích thước code | RAM overhead | Trạng thái |
|---------|-----------|--------------|--------|
| **ZeroClaw cơ bản** | 3.4MB | <5MB | ✅ Hiện tại |
| **+ Landlock** | +50KB | +100KB | ✅ Linux 5.13+ |
| **+ Firejail wrapper** | +20KB | +0KB (external) | ✅ Linux + firejail |
| **+ Memory monitoring** | +30KB | +50KB | ✅ Tất cả nền tảng |
| **+ Audit logging** | +40KB | +200KB (buffered) | ✅ Tất cả nền tảng |
| **Full security** | +140KB | +350KB | ✅ Vẫn <6MB tổng |

### Tương thích phần cứng $10

| Phần cứng | RAM | ZeroClaw (cơ bản) | ZeroClaw (full security) | Trạng thái |
|----------|-----|-----------------|--------------------------|--------|
| **Raspberry Pi Zero** | 512MB | ✅ 2% | ✅ 2.5% | Hoạt động |
| **Orange Pi Zero** | 512MB | ✅ 2% | ✅ 2.5% | Hoạt động |
| **NanoPi NEO** | 256MB | ✅ 4% | ✅ 5% | Hoạt động |
| **C.H.I.P.** | 512MB | ✅ 2% | ✅ 2.5% | Hoạt động |
| **Rock64** | 1GB | ✅ 1% | ✅ 1.2% | Hoạt động |

**Ngay cả với full security, ZeroClaw chỉ dùng <5% RAM trên board $10.**

---

## 5. Tính hoán đổi: mọi thứ vẫn pluggable

### Cam kết chính của ZeroClaw: hoán đổi bất kỳ thứ gì

```rust
// Providers (đã pluggable)
Box<dyn Provider>

// Channels (đã pluggable)
Box<dyn Channel>

// Memory (đã pluggable)
Box<dyn MemoryBackend>

// Tunnels (đã pluggable)
Box<dyn Tunnel>

// BÂY GIỜ CŨNG: Security (mới pluggable)
Box<dyn Sandbox>
Box<dyn Auditor>
Box<dyn ResourceMonitor>
```

### Hoán đổi security backend qua config

```toml
# Không dùng sandbox (nhanh nhất, chỉ app-layer)
[security.sandbox]
backend = "none"

# Dùng Landlock (Linux kernel LSM, native)
[security.sandbox]
backend = "landlock"

# Dùng Firejail (user-space, cần cài firejail)
[security.sandbox]
backend = "firejail"

# Dùng Docker (nặng nhất, cách ly hoàn toàn)
[security.sandbox]
backend = "docker"
```

**Giống như hoán đổi OpenAI sang Gemini, hay SQLite sang PostgreSQL.**

---

## 6. Tác động phụ thuộc: thêm tối thiểu

### Phụ thuộc hiện tại (để tham khảo)

```
reqwest, tokio, serde, anyhow, uuid, chrono, rusqlite,
axum, tracing, opentelemetry, ...
```

### Phụ thuộc của các security feature

| Tính năng | Phụ thuộc mới | Nền tảng |
|---------|------------------|----------|
| **Landlock** | `landlock` crate (pure Rust) | Chỉ Linux |
| **Firejail** | Không (binary ngoài) | Chỉ Linux |
| **Bubblewrap** | Không (binary ngoài) | macOS/Linux |
| **Docker** | `bollard` crate (Docker API) | Tất cả nền tảng |
| **Memory monitoring** | Không (std::alloc) | Tất cả nền tảng |
| **Audit logging** | Không (đã có hmac/sha2) | Tất cả nền tảng |

**Kết quả**: Hầu hết tính năng **không thêm phụ thuộc Rust mới** — chúng hoặc:
1. Dùng pure-Rust crate (landlock)
2. Bọc binary ngoài (Firejail, Bubblewrap)
3. Dùng phụ thuộc sẵn có (hmac, sha2 đã có trong Cargo.toml)

---

## Tóm tắt: các giá trị chính được bảo toàn

| Giá trị | Trước | Sau (có bảo mật) | Trạng thái |
|------------|--------|----------------------|--------|
| **<5MB RAM** | ✅ <5MB | ✅ <6MB (trường hợp xấu nhất) | ✅ Bảo toàn |
| **<10ms startup** | ✅ <10ms | ✅ <15ms (detection) | ✅ Bảo toàn |
| **3.4MB binary** | ✅ 3.4MB | ✅ 3.5MB (với tất cả features) | ✅ Bảo toàn |
| **ARM + x86 + RISC-V** | ✅ Tất cả | ✅ Tất cả | ✅ Bảo toàn |
| **Phần cứng $10** | ✅ Hoạt động | ✅ Hoạt động | ✅ Bảo toàn |
| **Pluggable everything** | ✅ Có | ✅ Có (cả bảo mật) | ✅ Cải thiện |
| **Cross-platform** | ✅ Có | ✅ Có | ✅ Bảo toàn |

---

## Điểm mấu chốt: feature flags + conditional compilation

```bash
# Developer build (nhanh nhất, không có extra feature)
cargo build --profile dev

# Standard release (build hiện tại của bạn)
cargo build --release

# Production với full security
cargo build --release --features security-full

# Nhắm đến phần cứng cụ thể
cargo build --release --target aarch64-unknown-linux-gnu  # Raspberry Pi
cargo build --release --target riscv64gc-unknown-linux-gnu # RISC-V
cargo build --release --target armv7-unknown-linux-gnueabihf  # ARMv7
```

**Mọi target, mọi nền tảng, mọi trường hợp sử dụng — vẫn nhanh, vẫn nhỏ, vẫn agnostic.**
