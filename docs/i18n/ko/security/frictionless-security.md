# 마찰 없는 보안: 설정 마법사에 대한 영향 제로

> ⚠️ **상태: 제안 / 로드맵**
>
> 이 문서는 제안된 접근 방식을 설명하며, 가상의 명령어나 설정을 포함할 수 있습니다.
> 현재 런타임 동작에 대해서는 [config-reference.md](../reference/api/config-reference.md), [operations-runbook.md](../ops/operations-runbook.md), [troubleshooting.md](../ops/troubleshooting.md)를 참고하십시오.

## 핵심 원칙

> **"보안 기능은 에어백과 같아야 합니다 — 존재하고, 보호하며, 필요할 때까지 보이지 않아야 합니다."**

## 설계: 자동 감지 (무음 모드)

### 1. 새로운 마법사 단계 없음 (9단계, 60초 미만 유지)

```rust
// 마법사는 변경되지 않음
// 보안 기능은 백그라운드에서 자동 감지

pub fn run_wizard() -> Result<Config> {
    // ... 기존 9단계, 변경 없음 ...

    let config = Config {
        // ... 기존 필드 ...

        // 새로 추가: 자동 감지된 보안 (마법사에 표시되지 않음)
        security: SecurityConfig::autodetect(),  // 무음!
    };

    config.save().await?;
    Ok(config)
}
```

### 2. 자동 감지 로직 (첫 시작 시 한 번 실행)

```rust
// src/security/detect.rs

impl SecurityConfig {
    /// 사용 가능한 sandbox를 감지하고 자동으로 활성화
    /// 플랫폼 + 사용 가능한 도구를 기반으로 스마트 기본값 반환
    pub fn autodetect() -> Self {
        Self {
            // Sandbox: Landlock(네이티브) 우선, 그다음 Firejail, 그다음 없음
            sandbox: SandboxConfig::autodetect(),

            // 리소스 제한: 항상 모니터링 활성화
            resources: ResourceLimits::default(),

            // 감사: 기본적으로 활성화, 설정 디렉토리에 로깅
            audit: AuditConfig::default(),

            // 나머지: 안전한 기본값
            ..SecurityConfig::default()
        }
    }
}

impl SandboxConfig {
    pub fn autodetect() -> Self {
        #[cfg(target_os = "linux")]
        {
            // Landlock 우선 (네이티브, 종속성 없음)
            if Self::probe_landlock() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Landlock,
                    ..Self::default()
                };
            }

            // 폴백: Firejail (설치된 경우)
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
            // macOS에서 Bubblewrap 시도
            if Self::probe_bubblewrap() {
                return Self {
                    enabled: true,
                    backend: SandboxBackend::Bubblewrap,
                    ..Self::default()
                };
            }
        }

        // 폴백: 비활성화 (그러나 애플리케이션 레이어 보안은 여전히 유지)
        Self {
            enabled: false,
            backend: SandboxBackend::None,
            ..Self::default()
        }
    }

    #[cfg(target_os = "linux")]
    fn probe_landlock() -> bool {
        // 최소한의 Landlock 규칙 집합 생성 시도
        // 성공하면 커널이 Landlock을 지원하는 것
        landlock::Ruleset::new()
            .set_access_fs(landlock::AccessFS::read_file)
            .add_path(Path::new("/tmp"), landlock::AccessFS::read_file)
            .map(|ruleset| ruleset.restrict_self().is_ok())
            .unwrap_or(false)
    }

    fn probe_firejail() -> bool {
        // firejail 명령어 존재 여부 확인
        std::process::Command::new("firejail")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
```

### 3. 첫 실행: 무음 로깅

```bash
$ zeroclaw agent -m "hello"

# 처음: 무음 감지
[INFO] Detecting security features...
[INFO] ✓ Landlock sandbox enabled (kernel 6.2+)
[INFO] ✓ Memory monitoring active (512MB limit)
[INFO] ✓ Audit logging enabled (~/.config/zeroclaw/audit.log)

# 이후 실행: 조용하게
$ zeroclaw agent -m "hello"
[agent] Thinking...
```

### 4. 설정 파일: 모든 기본값 숨김

```toml
# ~/.config/zeroclaw/config.toml

# 이 섹션들은 사용자가 커스터마이즈하지 않는 한 기록되지 않음
# [security.sandbox]
# enabled = true  # (기본값, 자동 감지)
# backend = "landlock"  # (기본값, 자동 감지)

# [security.resources]
# max_memory_mb = 512  # (기본값)

# [security.audit]
# enabled = true  # (기본값)
```

사용자가 변경할 때만:
```toml
[security.sandbox]
enabled = false  # 사용자가 명시적으로 비활성화

[security.resources]
max_memory_mb = 1024  # 사용자가 제한 증가
```

### 5. 고급 사용자: 명시적 제어

```bash
# 현재 활성 상태 확인
$ zeroclaw security --status
Security Status:
  ✓ Sandbox: Landlock (Linux kernel 6.2)
  ✓ Memory monitoring: 512MB limit
  ✓ Audit logging: ~/.config/zeroclaw/audit.log
  → 47 events logged today

# sandbox 명시적 비활성화 (설정에 기록)
$ zeroclaw config set security.sandbox.enabled false

# 특정 백엔드 활성화
$ zeroclaw config set security.sandbox.backend firejail

# 제한 조정
$ zeroclaw config set security.resources.max_memory_mb 2048
```

### 6. 우아한 성능 저하

| 플랫폼 | 최적 사용 가능 | 폴백 | 최악의 경우 |
|----------|---------------|----------|------------|
| **Linux 5.13+** | Landlock | None | 앱 레이어만 |
| **Linux (모든 버전)** | Firejail | Landlock | 앱 레이어만 |
| **macOS** | Bubblewrap | None | 앱 레이어만 |
| **Windows** | None | - | 앱 레이어만 |

**앱 레이어 보안은 항상 존재합니다** — 이것은 이미 포괄적인 기존 allowlist/경로 차단/인젝션 보호입니다.

---

## 설정 스키마 확장

```rust
// src/config/schema.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Sandbox 설정 (미설정 시 자동 감지)
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// 리소스 제한 (미설정 시 기본값 적용)
    #[serde(default)]
    pub resources: ResourceLimits,

    /// Audit logging (기본적으로 활성화)
    #[serde(default)]
    pub audit: AuditConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            sandbox: SandboxConfig::autodetect(),  // 무음 감지!
            resources: ResourceLimits::default(),
            audit: AuditConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Sandbox 활성화 (기본값: 자동 감지)
    #[serde(default)]
    pub enabled: Option<bool>,  // None = 자동 감지

    /// Sandbox 백엔드 (기본값: 자동 감지)
    #[serde(default)]
    pub backend: SandboxBackend,

    /// 사용자 정의 Firejail 인자 (선택 사항)
    #[serde(default)]
    pub firejail_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    Auto,       // 자동 감지 (기본값)
    Landlock,   // Linux 커널 LSM
    Firejail,   // 사용자 공간 sandbox
    Bubblewrap, // 사용자 네임스페이스
    Docker,     // 컨테이너 (무거움)
    None,       // 비활성화
}

impl Default for SandboxBackend {
    fn default() -> Self {
        Self::Auto  // 항상 기본적으로 자동 감지
    }
}
```

---

## 사용자 경험 비교

### 이전 (현재)

```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped
```

### 이후 (마찰 없는 보안 적용)

```bash
$ zeroclaw onboard
[1/9] Workspace Setup...
[2/9] AI Provider...
...
[9/9] Workspace Files...
✓ Security: Supervised | workspace-scoped | Landlock sandbox ✓
# ↑ 단 하나의 단어만 추가, 무음 자동 감지!
```

---

## 하위 호환성

| 시나리오 | 동작 |
|----------|----------|
| **기존 설정** | 변경 없이 동작, 새 기능은 선택 사용 |
| **새 설치** | 사용 가능한 보안을 자동 감지하고 활성화 |
| **sandbox 사용 불가** | 앱 레이어로 폴백 (여전히 안전) |
| **사용자 비활성화** | 설정 플래그 하나: `sandbox.enabled = false` |

---

## 요약

✅ **마법사에 대한 영향 제로** — 9단계, 60초 미만 유지
✅ **새 프롬프트 없음** — 무음 자동 감지
✅ **호환성 깨짐 없음** — 하위 호환
✅ **비활성화 가능** — 명시적 설정 플래그
✅ **상태 가시성** — `zeroclaw security --status`

마법사는 "빠른 범용 애플리케이션 설정"을 유지합니다 — 보안은 그저 **조용히 더 나아질** 뿐입니다.
