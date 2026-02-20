# Lộ Trình Cải Tiến Bảo Mật ZeroClaw

> ⚠️ **Trạng thái: Đề xuất / Lộ trình**
>
> Tài liệu này mô tả các hướng tiếp cận đề xuất và có thể bao gồm các lệnh hoặc cấu hình giả định.
> Để biết hành vi runtime hiện tại, xem [config-reference.md](../config-reference.md), [operations-runbook.md](../operations-runbook.md), và [troubleshooting.md](../troubleshooting.md).

## Trạng Thái Hiện Tại: Nền Tảng Vững Chắc

ZeroClaw đã có **application-layer security xuất sắc**:

✅ Command allowlist (không phải blocklist)
✅ Bảo vệ path traversal
✅ Chặn command injection (`$(...)`, backticks, `&&`, `>`)
✅ Cách ly secret (API key không bị rò rỉ ra shell)
✅ Rate limiting (20 actions/hour)
✅ Channel authorization (rỗng = từ chối tất cả, `*` = cho phép tất cả)
✅ Phân loại rủi ro (Low/Medium/High)
✅ Làm sạch biến môi trường
✅ Chặn forbidden paths
✅ Độ phủ kiểm thử toàn diện (1.017 test)

## Những Gì Còn Thiếu: Kiềm Chế Ở Cấp Độ OS

🔴 Chưa có sandboxing cấp OS (chroot, containers, namespaces)
🔴 Chưa có giới hạn tài nguyên (giới hạn CPU, memory, disk I/O)
🔴 Chưa có audit logging chống giả mạo
🔴 Chưa có syscall filtering (seccomp)

---

## So Sánh: ZeroClaw vs PicoClaw vs Production Grade

| Tính năng | PicoClaw | ZeroClaw Hiện Tại | ZeroClaw + Lộ Trình | Mục Tiêu Production |
|---------|----------|--------------|-------------------|-------------------|
| **Kích thước Binary** | ~8MB | **3.4MB** ✅ | 3.5-4MB | < 5MB |
| **RAM** | < 10MB | **< 5MB** ✅ | < 10MB | < 20MB |
| **Thời gian Startup** | < 1s | **< 10ms** ✅ | < 50ms | < 100ms |
| **Command Allowlist** | Không rõ | ✅ Có | ✅ Có | ✅ Có |
| **Path Blocking** | Không rõ | ✅ Có | ✅ Có | ✅ Có |
| **Injection Protection** | Không rõ | ✅ Có | ✅ Có | ✅ Có |
| **OS Sandbox** | Không | ❌ Không | ✅ Firejail/Landlock | ✅ Container/namespaces |
| **Resource Limits** | Không | ❌ Không | ✅ cgroups/Monitor | ✅ Full cgroups |
| **Audit Logging** | Không | ❌ Không | ✅ Ký HMAC | ✅ Tích hợp SIEM |
| **Điểm Bảo Mật** | C | **B+** | **A-** | **A+** |

---

## Lộ Trình Triển Khai

### Giai Đoạn 1: Kết Quả Nhanh (1-2 tuần)
**Mục tiêu**: Giải quyết các thiếu sót nghiêm trọng với độ phức tạp tối thiểu

| Nhiệm vụ | File | Công sức | Tác động |
|------|------|--------|-------|
| Landlock filesystem sandbox | `src/security/landlock.rs` | 2 ngày | Cao |
| Memory monitoring + OOM kill | `src/resources/memory.rs` | 1 ngày | Cao |
| CPU timeout mỗi lệnh | `src/tools/shell.rs` | 1 ngày | Cao |
| Audit logging cơ bản | `src/security/audit.rs` | 2 ngày | Trung bình |
| Cập nhật config schema | `src/config/schema.rs` | 1 ngày | - |

**Kết quả bàn giao**:
- Linux: Truy cập filesystem bị giới hạn trong workspace
- Tất cả nền tảng: Bảo vệ memory/CPU chống lệnh chạy vô hạn
- Tất cả nền tảng: Audit trail chống giả mạo

---

### Giai Đoạn 2: Tích Hợp Nền Tảng (2-3 tuần)
**Mục tiêu**: Tích hợp sâu với OS để cách ly cấp production

| Nhiệm vụ | Công sức | Tác động |
|------|--------|-------|
| Tự phát hiện Firejail + wrapping | 3 ngày | Rất cao |
| Bubblewrap wrapper cho macOS/*nix | 4 ngày | Rất cao |
| Tích hợp cgroups v2 systemd | 3 ngày | Cao |
| Syscall filtering với seccomp | 5 ngày | Cao |
| Audit log query CLI | 2 ngày | Trung bình |

**Kết quả bàn giao**:
- Linux: Cách ly hoàn toàn như container qua Firejail
- macOS: Cách ly filesystem với Bubblewrap
- Linux: Thực thi giới hạn tài nguyên qua cgroups
- Linux: Allowlist syscall

---

### Giai Đoạn 3: Hardening Production (1-2 tuần)
**Mục tiêu**: Các tính năng bảo mật doanh nghiệp

| Nhiệm vụ | Công sức | Tác động |
|------|--------|-------|
| Docker sandbox mode | 3 ngày | Cao |
| Certificate pinning cho channels | 2 ngày | Trung bình |
| Xác minh config đã ký | 2 ngày | Trung bình |
| Xuất audit tương thích SIEM | 2 ngày | Trung bình |
| Tự kiểm tra bảo mật (`zeroclaw audit --check`) | 1 ngày | Thấp |

**Kết quả bàn giao**:
- Tùy chọn cách ly thực thi dựa trên Docker
- HTTPS certificate pinning cho channel webhooks
- Xác minh chữ ký file config
- Xuất audit JSON/CSV cho phân tích ngoài

---

## Xem Trước Config Schema Mới

```toml
[security]
level = "strict"  # relaxed | default | strict | paranoid

# Cấu hình sandbox
[security.sandbox]
enabled = true
backend = "auto"  # auto | firejail | bubblewrap | landlock | docker | none

# Giới hạn tài nguyên
[resources]
max_memory_mb = 512
max_memory_per_command_mb = 128
max_cpu_percent = 50
max_cpu_time_seconds = 60
max_subprocesses = 10

# Audit logging
[security.audit]
enabled = true
log_path = "~/.config/zeroclaw/audit.log"
sign_events = true
max_size_mb = 100

# Autonomy (hiện có, được cải thiện)
[autonomy]
level = "supervised"  # readonly | supervised | full
allowed_commands = ["git", "ls", "cat", "grep", "find"]
forbidden_paths = ["/etc", "/root", "~/.ssh"]
require_approval_for_medium_risk = true
block_high_risk_commands = true
max_actions_per_hour = 20
```

---

## Xem Trước Lệnh CLI

```bash
# Kiểm tra trạng thái bảo mật
zeroclaw security --check
# → ✓ Sandbox: Firejail active
# → ✓ Audit logging enabled (42 events today)
# → → Resource limits: 512MB mem, 50% CPU

# Truy vấn audit log
zeroclaw audit --user @alice --since 24h
zeroclaw audit --risk high --violations-only
zeroclaw audit --verify-signatures

# Kiểm tra sandbox
zeroclaw sandbox --test
# → Testing isolation...
#   ✓ Cannot read /etc/passwd
#   ✓ Cannot access ~/.ssh
#   ✓ Can read /workspace
```

---

## Tóm Tắt

**ZeroClaw đã an toàn hơn PicoClaw** với:
- Binary nhỏ hơn 50% (3.4MB so với 8MB)
- RAM ít hơn 50% (< 5MB so với < 10MB)
- Startup nhanh hơn 100 lần (< 10ms so với < 1s)
- Policy engine bảo mật toàn diện
- Độ phủ kiểm thử rộng

**Khi triển khai lộ trình này**, ZeroClaw sẽ trở thành:
- Cấp production với OS-level sandboxing
- Nhận biết tài nguyên với bảo vệ memory/CPU
- Sẵn sàng audit với logging chống giả mạo
- Sẵn sàng doanh nghiệp với các cấp độ bảo mật có thể cấu hình

**Công sức ước tính**: 4-7 tuần để triển khai đầy đủ
**Giá trị**: Biến ZeroClaw từ "an toàn để kiểm thử" thành "an toàn cho production"
