# Resource Limits for ZeroClaw

## Problem
ZeroClaw has rate limiting (20 actions/hour) but no resource caps. A runaway agent could:
- Exhaust available memory
- Spin CPU at 100%
- Fill disk with logs/output

---

## Proposed Solutions

### Option 1: cgroups v2 (Linux, Recommended)
Automatically create a cgroup for zeroclaw with limits.

```bash
# Create systemd service with limits
[Service]
MemoryMax=512M
CPUQuota=100%
IOReadBandwidthMax=/dev/sda 10M
IOWriteBandwidthMax=/dev/sda 10M
TasksMax=100
```

### Option 2: tokio::task::deadlock detection
Prevent task starvation.

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

### Option 3: Memory monitoring
Track heap usage and kill if over limit.

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

## Config Schema

```toml
[resources]
# Memory limits (in MB)
max_memory_mb = 512
max_memory_per_command_mb = 128

# CPU limits
max_cpu_percent = 50
max_cpu_time_seconds = 60

# Disk I/O limits
max_log_size_mb = 100
max_temp_storage_mb = 500

# Process limits
max_subprocesses = 10
max_open_files = 100
```

---

## Implementation Priority

| Phase | Feature | Effort | Impact |
|-------|---------|--------|--------|
| **P0** | Memory monitoring + kill | Low | High |
| **P1** | CPU timeout per command | Low | High |
| **P2** | cgroups integration (Linux) | Medium | Very High |
| **P3** | Disk I/O limits | Medium | Medium |
