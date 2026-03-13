# ZeroClaw 资源限制

> ⚠️ **状态：提案 / 路线图**
>
> 本文档描述提议的实现方法，可能包含假设的命令或配置。
> 如需了解当前运行时行为，请参见 [config-reference.zh-CN.md](../reference/api/config-reference.zh-CN.md)、[operations-runbook.zh-CN.md](operations-runbook.zh-CN.md) 和 [troubleshooting.zh-CN.md](troubleshooting.zh-CN.md)。

## 问题

ZeroClaw 具有速率限制（每小时 20 个操作），但没有资源上限。失控的代理可能会：
- 耗尽可用内存
- CPU 占用 100%
- 日志/输出填满磁盘

---

## 提议的解决方案

### 选项 1：cgroups v2（Linux，推荐）

自动为 zeroclaw 创建带有限制的 cgroup。

```bash
# 创建带有限制的 systemd 服务
[Service]
MemoryMax=512M
CPUQuota=100%
IOReadBandwidthMax=/dev/sda 10M
IOWriteBandwidthMax=/dev/sda 10M
TasksMax=100
```

### 选项 2：tokio::task::死锁检测

防止任务饥饿。

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
    // CPU 超时
    timeout(cpu_time_limit, fut).await?
}
```

### 选项 3：内存监控

跟踪堆使用情况，超过限制则终止。

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

## 配置模式

```toml
[resources]
# 内存限制（单位 MB）
max_memory_mb = 512
max_memory_per_command_mb = 128

# CPU 限制
max_cpu_percent = 50
max_cpu_time_seconds = 60

# 磁盘 I/O 限制
max_log_size_mb = 100
max_temp_storage_mb = 500

# 进程限制
max_subprocesses = 10
max_open_files = 100
```

---

## 实现优先级

| 阶段 | 功能 | 工作量 | 影响 |
|-------|---------|--------|--------|
| **P0** | 内存监控 + 终止 | 低 | 高 |
| **P1** | 每个命令的 CPU 超时 | 低 | 高 |
| **P2** | cgroups 集成（Linux） | 中 | 极高 |
| **P3** | 磁盘 I/O 限制 | 中 | 中 |
