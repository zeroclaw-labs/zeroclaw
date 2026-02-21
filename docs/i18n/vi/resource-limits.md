# Giới hạn tài nguyên

> ⚠️ **Trạng thái: Đề xuất / Lộ trình**
>
> Tài liệu này mô tả các hướng tiếp cận đề xuất và có thể bao gồm các lệnh hoặc cấu hình giả định.
> Để biết hành vi runtime hiện tại, xem [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md), và [troubleshooting.md](troubleshooting.md).

## Vấn đề

ZeroClaw có rate limiting (20 actions/hour) nhưng chưa có giới hạn tài nguyên. Một agent bị lỗi lặp vòng có thể:
- Làm cạn kiệt bộ nhớ khả dụng
- Quay CPU liên tục ở 100%
- Lấp đầy ổ đĩa bằng log/output

---

## Các giải pháp đề xuất

### Tùy chọn 1: cgroups v2 (Linux, khuyến nghị)

Tự động tạo cgroup cho zeroclaw với các giới hạn.

```bash
# Tạo systemd service với giới hạn
[Service]
MemoryMax=512M
CPUQuota=100%
IOReadBandwidthMax=/dev/sda 10M
IOWriteBandwidthMax=/dev/sda 10M
TasksMax=100
```

### Tùy chọn 2: phát hiện deadlock với tokio::task

Ngăn task starvation.

```rust
use tokio::time::{timeout, Duration};

pub async fn execute_with_timeout<F, T>(
    fut: F,
    cpu_time_limit: Duration,
    memory_limit: usize,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    // CPU timeout
    timeout(cpu_time_limit, fut).await?
}
```

### Tùy chọn 3: memory monitoring

Theo dõi sử dụng heap và kill nếu vượt giới hạn.

```rust
use std::alloc::{GlobalAlloc, Layout, System};

struct LimitedAllocator<A> {
    inner: A,
    max_bytes: usize,
    used: std::sync::atomic::AtomicUsize,
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for LimitedAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let current = self.used.fetch_add(layout.size(), std::sync::atomic::Ordering::Relaxed);
        if current + layout.size() > self.max_bytes {
            std::process::abort();
        }
        self.inner.alloc(layout)
    }
}
```

---

## Config schema

```toml
[resources]
# Giới hạn bộ nhớ (tính bằng MB)
max_memory_mb = 512
max_memory_per_command_mb = 128

# Giới hạn CPU
max_cpu_percent = 50
max_cpu_time_seconds = 60

# Giới hạn Disk I/O
max_log_size_mb = 100
max_temp_storage_mb = 500

# Giới hạn process
max_subprocesses = 10
max_open_files = 100
```

---

## Thứ tự triển khai

| Giai đoạn | Tính năng | Công sức | Tác động |
|-------|---------|--------|--------|
| **P0** | Memory monitoring + kill | Thấp | Cao |
| **P1** | CPU timeout mỗi lệnh | Thấp | Cao |
| **P2** | Tích hợp cgroups (Linux) | Trung bình | Rất cao |
| **P3** | Giới hạn Disk I/O | Trung bình | Trung bình |
