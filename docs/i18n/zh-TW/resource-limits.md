# ZeroClaw 資源限制（繁體中文）

> ⚠️ **狀態：提案 / 規劃路線**
>
> 本文件描述提案中的方法，可能包含假設性的指令或設定。
> 有關目前的執行期行為，請參閱 [config-reference.md](config-reference.md)、[operations-runbook.md](operations-runbook.md) 及 [troubleshooting.md](troubleshooting.md)。

## 問題

ZeroClaw 已有速率限制（每小時 20 個動作），但沒有資源上限。失控的代理可能會：
- 耗盡可用記憶體
- 將 CPU 跑到 100%
- 用日誌/輸出塞滿磁碟

---

## 提議方案

### 方案 1：cgroups v2（Linux，推薦方案）

自動為 ZeroClaw 建立一個帶有限制的 cgroup。

```bash
# 建立附帶限制的 systemd 服務
[Service]
MemoryMax=512M
CPUQuota=100%
IOReadBandwidthMax=/dev/sda 10M
IOWriteBandwidthMax=/dev/sda 10M
TasksMax=100
```

### 方案 2：tokio::task::deadlock 偵測

防止任務飢餓。

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
    // CPU 逾時
    timeout(cpu_time_limit, fut).await?
}
```

### 方案 3：記憶體監控

追蹤堆積記憶體使用量，超過限制時終止程式。

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

## 設定架構

```toml
[resources]
# 記憶體限制（單位：MB）
max_memory_mb = 512
max_memory_per_command_mb = 128

# CPU 限制
max_cpu_percent = 50
max_cpu_time_seconds = 60

# 磁碟 I/O 限制
max_log_size_mb = 100
max_temp_storage_mb = 500

# 行程限制
max_subprocesses = 10
max_open_files = 100
```

---

## 實作優先順序

| 階段 | 功能 | 工作量 | 影響程度 |
|------|------|--------|---------|
| **P0** | 記憶體監控 + 終止機制 | 低 | 高 |
| **P1** | 每個指令的 CPU 逾時 | 低 | 高 |
| **P2** | cgroups 整合（Linux） | 中 | 非常高 |
| **P3** | 磁碟 I/O 限制 | 中 | 中 |
