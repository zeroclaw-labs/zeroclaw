# Chiến lược sandboxing

> ⚠️ **Trạng thái: Đề xuất / Lộ trình**
>
> Tài liệu này mô tả các hướng tiếp cận đề xuất và có thể bao gồm các lệnh hoặc cấu hình giả định.
> Để biết hành vi runtime hiện tại, xem [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md), và [troubleshooting.md](troubleshooting.md).

## Vấn đề

ZeroClaw hiện có application-layer security (allowlists, path blocking, command injection protection) nhưng thiếu cơ chế cách ly cấp hệ điều hành. Nếu kẻ tấn công nằm trong allowlist, họ có thể chạy bất kỳ lệnh nào được cho phép với quyền của user zeroclaw.

## Các giải pháp đề xuất

### Tùy chọn 1: tích hợp Firejail (khuyến nghị cho Linux)

Firejail cung cấp sandboxing ở user-space với overhead tối thiểu.

```rust
// src/security/firejail.rs
use std::process::Command;

pub struct FirejailSandbox {
    enabled: bool,
}

impl FirejailSandbox {
    pub fn new() -> Self {
        let enabled = which::which("firejail").is_ok();
        Self { enabled }
    }

    pub fn wrap_command(&self, cmd: &mut Command) -> &mut Command {
        if !self.enabled {
            return cmd;
        }

        // Firejail bọc bất kỳ lệnh nào với sandboxing
        let mut jail = Command::new("firejail");
        jail.args([
            "--private=home",           // Thư mục home mới
            "--private-dev",            // /dev tối giản
            "--nosound",                // Không âm thanh
            "--no3d",                   // Không tăng tốc 3D
            "--novideo",                // Không thiết bị video
            "--nowheel",                // Không thiết bị nhập liệu
            "--notv",                   // Không thiết bị TV
            "--noprofile",              // Bỏ qua tải profile
            "--quiet",                  // Tắt cảnh báo
        ]);

        // Gắn thêm lệnh gốc
        if let Some(program) = cmd.get_program().to_str() {
            jail.arg(program);
        }
        for arg in cmd.get_args() {
            if let Some(s) = arg.to_str() {
                jail.arg(s);
            }
        }

        // Thay thế lệnh gốc bằng firejail wrapper
        *cmd = jail;
        cmd
    }
}
```

**Tùy chọn config:**
```toml
[security]
enable_sandbox = true
sandbox_backend = "firejail"  # hoặc "none", "bubblewrap", "docker"
```

---

### Tùy chọn 2: Bubblewrap (di động, không cần root)

Bubblewrap dùng user namespaces để tạo container.

```bash
# Cài bubblewrap
sudo apt install bubblewrap

# Bọc lệnh:
bwrap --ro-bind /usr /usr \
      --dev /dev \
      --proc /proc \
      --bind /workspace /workspace \
      --unshare-all \
      --share-net \
      --die-with-parent \
      -- /bin/sh -c "command"
```

---

### Tùy chọn 3: Docker-in-Docker (nặng nhưng cách ly hoàn toàn)

Chạy các công cụ agent trong container tạm thời.

```rust
pub struct DockerSandbox {
    image: String,
}

impl DockerSandbox {
    pub async fn execute(&self, command: &str, workspace: &Path) -> Result<String> {
        let output = Command::new("docker")
            .args([
                "run", "--rm",
                "--memory", "512m",
                "--cpus", "1.0",
                "--network", "none",
                "--volume", &format!("{}:/workspace", workspace.display()),
                &self.image,
                "sh", "-c", command
            ])
            .output()
            .await?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
```

---

### Tùy chọn 4: Landlock (Linux kernel LSM, Rust native)

Landlock cung cấp kiểm soát truy cập hệ thống file mà không cần container.

```rust
use landlock::{Ruleset, AccessFS};

pub fn apply_landlock() -> Result<()> {
    let ruleset = Ruleset::new()
        .set_access_fs(AccessFS::read_file | AccessFS::write_file)
        .add_path(Path::new("/workspace"), AccessFS::read_file | AccessFS::write_file)?
        .add_path(Path::new("/tmp"), AccessFS::read_file | AccessFS::write_file)?
        .restrict_self()?;

    Ok(())
}
```

---

## Thứ tự triển khai ưu tiên

| Giai đoạn | Giải pháp | Công sức | Tăng cường bảo mật |
|-------|----------|--------|---------------|
| **P0** | Landlock (chỉ Linux, native) | Thấp | Cao (filesystem) |
| **P1** | Tích hợp Firejail | Thấp | Rất cao |
| **P2** | Bubblewrap wrapper | Trung bình | Rất cao |
| **P3** | Docker sandbox mode | Cao | Hoàn toàn |

## Mở rộng config schema

```toml
[security.sandbox]
enabled = true
backend = "auto"  # auto | firejail | bubblewrap | landlock | docker | none

# Dành riêng cho Firejail
[security.sandbox.firejail]
extra_args = ["--seccomp", "--caps.drop=all"]

# Dành riêng cho Landlock
[security.sandbox.landlock]
readonly_paths = ["/usr", "/bin", "/lib"]
readwrite_paths = ["$HOME/workspace", "/tmp/zeroclaw"]
```

## Chiến lược kiểm thử

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_blocks_path_traversal() {
        // Thử đọc /etc/passwd qua sandbox
        let result = sandboxed_execute("cat /etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_allows_workspace_access() {
        let result = sandboxed_execute("ls /workspace");
        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_no_network_isolation() {
        // Đảm bảo mạng bị chặn khi được cấu hình
        let result = sandboxed_execute("curl http://example.com");
        assert!(result.is_err());
    }
}
```
