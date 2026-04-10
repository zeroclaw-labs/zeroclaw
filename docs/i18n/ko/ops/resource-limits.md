# ZeroClaw 리소스 제한

> ⚠️ **상태: 제안 / 로드맵**
>
> 이 문서는 제안된 접근 방식을 설명하며 가상의 명령어나 설정을 포함할 수 있습니다.
> 현재 런타임 동작은 [config-reference.md](../reference/api/config-reference.md), [operations-runbook.md](operations-runbook.md), [troubleshooting.md](troubleshooting.md)를 참조하십시오.

## 문제

ZeroClaw에는 속도 제한(시간당 20회 액션)이 있지만 리소스 상한은 없습니다. 폭주하는 에이전트가 다음과 같은 문제를 일으킬 수 있습니다:
- 사용 가능한 메모리 고갈
- CPU 100% 점유
- 로그/출력으로 디스크 가득 채움

---

## 제안된 해결 방안

### 옵션 1: cgroups v2 (Linux, 권장)

ZeroClaw를 위한 cgroup을 자동 생성하여 제한을 적용합니다.

```bash
# 제한이 적용된 systemd 서비스 생성
[Service]
MemoryMax=512M
CPUQuota=100%
IOReadBandwidthMax=/dev/sda 10M
IOWriteBandwidthMax=/dev/sda 10M
TasksMax=100
```

### 옵션 2: tokio::task::deadlock 감지

태스크 기아(starvation)를 방지합니다.

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
    // CPU 타임아웃
    timeout(cpu_time_limit, fut).await?
}
```

### 옵션 3: 메모리 모니터링

힙 사용량을 추적하고 제한을 초과하면 종료합니다.

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

## 설정 스키마

```toml
[resources]
# 메모리 제한 (MB 단위)
max_memory_mb = 512
max_memory_per_command_mb = 128

# CPU 제한
max_cpu_percent = 50
max_cpu_time_seconds = 60

# 디스크 I/O 제한
max_log_size_mb = 100
max_temp_storage_mb = 500

# 프로세스 제한
max_subprocesses = 10
max_open_files = 100
```

---

## 구현 우선순위

| 단계 | 기능 | 난이도 | 영향도 |
|-------|---------|--------|--------|
| **P0** | 메모리 모니터링 + 종료 | 낮음 | 높음 |
| **P1** | 명령어별 CPU 타임아웃 | 낮음 | 높음 |
| **P2** | cgroups 통합 (Linux) | 중간 | 매우 높음 |
| **P3** | 디스크 I/O 제한 | 중간 | 중간 |
