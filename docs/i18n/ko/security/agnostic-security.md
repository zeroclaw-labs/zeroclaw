# 플랫폼 무관 보안: 이식성에 대한 영향 제로

> ⚠️ **상태: 제안 / 로드맵**
>
> 이 문서는 제안된 접근 방식을 설명하며, 가상의 명령어나 설정을 포함할 수 있습니다.
> 현재 런타임 동작에 대해서는 [config-reference.md](../reference/api/config-reference.md), [operations-runbook.md](../ops/operations-runbook.md), [troubleshooting.md](../ops/troubleshooting.md)를 참고하십시오.

## 핵심 질문: 보안 기능이 다음을 저해하는가...

1. ❓ 빠른 크로스 컴파일 빌드?
2. ❓ 플러그 가능한 아키텍처 (모든 것을 교체 가능)?
3. ❓ 하드웨어 무관성 (ARM, x86, RISC-V)?
4. ❓ 소형 하드웨어 지원 (5MB 미만 RAM, $10 보드)?

**답변: 모두 아닙니다** — 보안은 **선택적 feature flag**와 **플랫폼별 조건부 컴파일**로 설계되었습니다.

---

## 1. 빌드 속도: Feature 게이트 방식의 보안

### Cargo.toml: Feature 뒤에 숨겨진 보안 기능

```toml
[features]
default = ["basic-security"]

# 기본 보안 (항상 활성화, 오버헤드 제로)
basic-security = []

# 플랫폼별 sandbox (플랫폼별 선택 사용)
sandbox-landlock = []   # Linux 전용
sandbox-firejail = []  # Linux 전용
sandbox-bubblewrap = []# macOS/Linux
sandbox-docker = []    # 모든 플랫폼 (무거움)

# 전체 보안 스위트 (프로덕션 빌드용)
security-full = [
    "basic-security",
    "sandbox-landlock",
    "resource-monitoring",
    "audit-logging",
]

# 리소스 및 감사 모니터링
resource-monitoring = []
audit-logging = []

# 개발 빌드 (가장 빠름, 추가 종속성 없음)
dev = []
```

### 빌드 명령어 (프로파일 선택)

```bash
# 초고속 개발 빌드 (보안 추가 기능 없음)
cargo build --profile dev

# 기본 보안 포함 릴리스 빌드 (기본값)
cargo build --release
# → 포함: allowlist, 경로 차단, 인젝션 보호
# → 제외: Landlock, Firejail, audit logging

# 전체 보안 포함 프로덕션 빌드
cargo build --release --features security-full
# → 포함: 모든 기능

# 플랫폼별 sandbox만 포함
cargo build --release --features sandbox-landlock  # Linux
cargo build --release --features sandbox-docker    # 모든 플랫폼
```

### 조건부 컴파일: 비활성화 시 오버헤드 제로

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

// 기본 보안은 항상 포함 (feature flag 불필요)
pub mod policy;  // allowlist, 경로 차단, 인젝션 보호
```

**결과**: Feature가 비활성화되면 코드 자체가 컴파일되지 않습니다 — **바이너리 크기 증가 제로**.

---

## 2. 플러그 가능한 아키텍처: 보안도 Trait입니다

### 보안 백엔드 Trait (다른 모든 것처럼 교체 가능)

```rust
// src/security/traits.rs

#[async_trait]
pub trait Sandbox: Send + Sync {
    /// 명령어를 sandbox 보호로 래핑
    fn wrap_command(&self, cmd: &mut std::process::Command) -> std::io::Result<()>;

    /// 이 플랫폼에서 sandbox 사용 가능 여부 확인
    fn is_available(&self) -> bool;

    /// 사람이 읽을 수 있는 이름
    fn name(&self) -> &str;
}

// No-op sandbox (항상 사용 가능)
pub struct NoopSandbox;

impl Sandbox for NoopSandbox {
    fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
        Ok(())  // 변경 없이 통과
    }

    fn is_available(&self) -> bool { true }
    fn name(&self) -> &str { "none" }
}
```

### Factory 패턴: Feature 기반 자동 선택

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

    // 폴백: 항상 사용 가능
    Box::new(NoopSandbox)
}
```

**Provider, Channel, Memory와 마찬가지로 — 보안도 플러그 가능합니다!**

---

## 3. 하드웨어 무관성: 동일한 바이너리, 다른 플랫폼

### 크로스 플랫폼 동작 매트릭스

| 플랫폼 | 빌드 가능 | 런타임 동작 |
|----------|-----------|------------------|
| **Linux ARM** (Raspberry Pi) | ✅ 예 | Landlock → None (우아한 폴백) |
| **Linux x86_64** | ✅ 예 | Landlock → Firejail → None |
| **macOS ARM** (M1/M2) | ✅ 예 | Bubblewrap → None |
| **macOS x86_64** | ✅ 예 | Bubblewrap → None |
| **Windows ARM** | ✅ 예 | None (앱 레이어) |
| **Windows x86_64** | ✅ 예 | None (앱 레이어) |
| **RISC-V Linux** | ✅ 예 | Landlock → None |

### 작동 원리: 런타임 감지

```rust
// src/security/detect.rs

impl SandboxingStrategy {
    /// 런타임에 사용 가능한 최적의 sandbox 선택
    pub fn detect() -> SandboxingStrategy {
        #[cfg(target_os = "linux")]
        {
            // Landlock을 먼저 시도 (커널 기능 감지)
            if Self::probe_landlock() {
                return SandboxingStrategy::Landlock;
            }

            // Firejail 시도 (사용자 공간 도구 감지)
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

        // 항상 사용 가능한 폴백
        SandboxingStrategy::ApplicationLayer
    }
}
```

**동일한 바이너리가 어디서나 실행됩니다** — 사용 가능한 기능에 따라 보호 수준을 자동으로 조정합니다.

---

## 4. 소형 하드웨어: 메모리 영향 분석

### 바이너리 크기 영향 (추정)

| 기능 | 코드 크기 | RAM 오버헤드 | 상태 |
|---------|-----------|--------------|--------|
| **기본 ZeroClaw** | 3.4MB | <5MB | ✅ 현재 |
| **+ Landlock** | +50KB | +100KB | ✅ Linux 5.13+ |
| **+ Firejail 래퍼** | +20KB | +0KB (외부) | ✅ Linux + firejail |
| **+ 메모리 모니터링** | +30KB | +50KB | ✅ 모든 플랫폼 |
| **+ Audit logging** | +40KB | +200KB (버퍼링) | ✅ 모든 플랫폼 |
| **전체 보안** | +140KB | +350KB | ✅ 총 6MB 미만 유지 |

### $10 하드웨어 호환성

| 하드웨어 | RAM | ZeroClaw (기본) | ZeroClaw (전체 보안) | 상태 |
|----------|-----|-----------------|--------------------------|--------|
| **Raspberry Pi Zero** | 512MB | ✅ 2% | ✅ 2.5% | 동작 |
| **Orange Pi Zero** | 512MB | ✅ 2% | ✅ 2.5% | 동작 |
| **NanoPi NEO** | 256MB | ✅ 4% | ✅ 5% | 동작 |
| **C.H.I.P.** | 512MB | ✅ 2% | ✅ 2.5% | 동작 |
| **Rock64** | 1GB | ✅ 1% | ✅ 1.2% | 동작 |

**전체 보안을 적용하더라도 ZeroClaw는 $10 보드에서 RAM의 5% 미만을 사용합니다.**

---

## 5. 플랫폼 무관 교체: 모든 것이 플러그 가능하게 유지됩니다

### ZeroClaw의 핵심 약속: 모든 것을 교체 가능

```rust
// Provider (이미 플러그 가능)
Box<dyn Provider>

// Channel (이미 플러그 가능)
Box<dyn Channel>

// Memory (이미 플러그 가능)
Box<dyn MemoryBackend>

// Tunnel (이미 플러그 가능)
Box<dyn Tunnel>

// 새로 추가: 보안 (새롭게 플러그 가능)
Box<dyn Sandbox>
Box<dyn Auditor>
Box<dyn ResourceMonitor>
```

### 설정으로 보안 백엔드 교체

```toml
# sandbox 없음 (가장 빠름, 앱 레이어만)
[security.sandbox]
backend = "none"

# Landlock 사용 (Linux 커널 LSM, 네이티브)
[security.sandbox]
backend = "landlock"

# Firejail 사용 (사용자 공간, firejail 설치 필요)
[security.sandbox]
backend = "firejail"

# Docker 사용 (가장 무거움, 가장 높은 격리)
[security.sandbox]
backend = "docker"
```

**OpenAI를 Gemini로, SQLite를 PostgreSQL로 교체하는 것과 동일합니다.**

---

## 6. 종속성 영향: 최소한의 새 종속성

### 현재 종속성 (참고용)

```
reqwest, tokio, serde, anyhow, uuid, chrono, rusqlite,
axum, tracing, opentelemetry, ...
```

### 보안 기능 종속성

| 기능 | 새 종속성 | 플랫폼 |
|---------|------------------|----------|
| **Landlock** | `landlock` crate (순수 Rust) | Linux 전용 |
| **Firejail** | 없음 (외부 바이너리) | Linux 전용 |
| **Bubblewrap** | 없음 (외부 바이너리) | macOS/Linux |
| **Docker** | `bollard` crate (Docker API) | 모든 플랫폼 |
| **메모리 모니터링** | 없음 (std::alloc) | 모든 플랫폼 |
| **Audit logging** | 없음 (hmac/sha2 이미 보유) | 모든 플랫폼 |

**결과**: 대부분의 기능은 **새로운 Rust 종속성을 추가하지 않습니다** — 다음 중 하나에 해당합니다:
1. 순수 Rust crate 사용 (landlock)
2. 외부 바이너리 래핑 (Firejail, Bubblewrap)
3. 기존 종속성 사용 (hmac, sha2가 이미 Cargo.toml에 포함)

---

## 요약: 핵심 가치 제안 보존

| 가치 제안 | 이전 | 이후 (보안 포함) | 상태 |
|------------|--------|----------------------|--------|
| **5MB 미만 RAM** | ✅ <5MB | ✅ <6MB (최악의 경우) | ✅ 보존 |
| **10ms 미만 시작 시간** | ✅ <10ms | ✅ <15ms (감지 포함) | ✅ 보존 |
| **3.4MB 바이너리** | ✅ 3.4MB | ✅ 3.5MB (모든 기능 포함) | ✅ 보존 |
| **ARM + x86 + RISC-V** | ✅ 전체 | ✅ 전체 | ✅ 보존 |
| **$10 하드웨어** | ✅ 동작 | ✅ 동작 | ✅ 보존 |
| **모든 것 플러그 가능** | ✅ 예 | ✅ 예 (보안 포함) | ✅ 향상 |
| **크로스 플랫폼** | ✅ 예 | ✅ 예 | ✅ 보존 |

---

## 핵심: Feature Flag + 조건부 컴파일

```bash
# 개발자 빌드 (가장 빠름, 추가 기능 없음)
cargo build --profile dev

# 표준 릴리스 (현재 빌드)
cargo build --release

# 전체 보안 포함 프로덕션
cargo build --release --features security-full

# 특정 하드웨어 대상
cargo build --release --target aarch64-unknown-linux-gnu  # Raspberry Pi
cargo build --release --target riscv64gc-unknown-linux-gnu # RISC-V
cargo build --release --target armv7-unknown-linux-gnueabihf  # ARMv7
```

**모든 타겟, 모든 플랫폼, 모든 사용 사례 — 여전히 빠르고, 여전히 작고, 여전히 플랫폼에 무관합니다.**
