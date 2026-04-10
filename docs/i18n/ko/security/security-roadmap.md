# ZeroClaw 보안 개선 로드맵

> ⚠️ **상태: 제안 / 로드맵**
>
> 이 문서는 제안된 접근 방식을 설명하며, 가상의 명령어나 설정을 포함할 수 있습니다.
> 현재 런타임 동작에 대해서는 [config-reference.md](../reference/api/config-reference.md), [operations-runbook.md](../ops/operations-runbook.md), [troubleshooting.md](../ops/troubleshooting.md)를 참고하십시오.

## 현재 상태: 견고한 기반

ZeroClaw는 이미 **우수한 애플리케이션 레이어 보안**을 갖추고 있습니다:

✅ 명령어 allowlist (blocklist가 아님)
✅ 경로 순회 보호
✅ 명령어 인젝션 차단 (`$(...)`, 백틱, `&&`, `>`)
✅ 시크릿 격리 (API 키가 셸에 유출되지 않음)
✅ 속도 제한 (시간당 20회)
✅ 채널 인가 (비어있으면 = 전체 거부, `*` = 전체 허용)
✅ 위험도 분류 (낮음/중간/높음)
✅ 환경 변수 정리
✅ 금지 경로 차단
✅ 포괄적 테스트 커버리지 (1,017개 테스트)

## 부족한 부분: OS 레벨 격리

🔴 OS 레벨 sandbox 없음 (chroot, 컨테이너, 네임스페이스)
🔴 리소스 제한 없음 (CPU, 메모리, 디스크 I/O 상한)
🔴 변조 방지 audit logging 없음
🔴 시스템 콜 필터링 없음 (seccomp)

---

## 비교: ZeroClaw vs PicoClaw vs 프로덕션 등급

| 기능 | PicoClaw | 현재 ZeroClaw | ZeroClaw + 로드맵 | 프로덕션 목표 |
|---------|----------|--------------|-------------------|-------------------|
| **바이너리 크기** | ~8MB | **3.4MB** ✅ | 3.5-4MB | < 5MB |
| **RAM 사용량** | < 10MB | **< 5MB** ✅ | < 10MB | < 20MB |
| **시작 시간** | < 1s | **< 10ms** ✅ | < 50ms | < 100ms |
| **명령어 Allowlist** | 불명 | ✅ 예 | ✅ 예 | ✅ 예 |
| **경로 차단** | 불명 | ✅ 예 | ✅ 예 | ✅ 예 |
| **인젝션 보호** | 불명 | ✅ 예 | ✅ 예 | ✅ 예 |
| **OS Sandbox** | 없음 | ❌ 없음 | ✅ Firejail/Landlock | ✅ 컨테이너/네임스페이스 |
| **리소스 제한** | 없음 | ❌ 없음 | ✅ cgroups/모니터 | ✅ 전체 cgroups |
| **Audit Logging** | 없음 | ❌ 없음 | ✅ HMAC 서명 | ✅ SIEM 통합 |
| **보안 점수** | C | **B+** | **A-** | **A+** |

---

## 구현 로드맵

### 1단계: 빠른 성과 (1-2주)
**목표**: 최소한의 복잡성으로 핵심 격차 해소

| 작업 | 파일 | 노력 | 영향 |
|------|------|--------|-------|
| Landlock 파일 시스템 sandbox | `src/security/landlock.rs` | 2일 | 높음 |
| 메모리 모니터링 + OOM 종료 | `src/resources/memory.rs` | 1일 | 높음 |
| 명령어별 CPU 타임아웃 | `src/tools/shell.rs` | 1일 | 높음 |
| 기본 audit logging | `src/security/audit.rs` | 2일 | 중간 |
| 설정 스키마 업데이트 | `src/config/schema.rs` | 1일 | - |

**결과물**:
- Linux: 파일 시스템 접근이 워크스페이스로 제한됨
- 모든 플랫폼: 폭주하는 명령어에 대한 메모리/CPU 가드
- 모든 플랫폼: 변조 방지 감사 추적

---

### 2단계: 플랫폼 통합 (2-3주)
**목표**: 프로덕션 등급 격리를 위한 심층 OS 통합

| 작업 | 노력 | 영향 |
|------|--------|-------|
| Firejail 자동 감지 + 래핑 | 3일 | 매우 높음 |
| macOS/*nix용 Bubblewrap 래퍼 | 4일 | 매우 높음 |
| cgroups v2 systemd 통합 | 3일 | 높음 |
| seccomp 시스템 콜 필터링 | 5일 | 높음 |
| Audit log 쿼리 CLI | 2일 | 중간 |

**결과물**:
- Linux: Firejail을 통한 완전한 컨테이너급 격리
- macOS: Bubblewrap 파일 시스템 격리
- Linux: cgroups 리소스 적용
- Linux: 시스템 콜 allowlist

---

### 3단계: 프로덕션 강화 (1-2주)
**목표**: 엔터프라이즈 보안 기능

| 작업 | 노력 | 영향 |
|------|--------|-------|
| Docker sandbox 모드 옵션 | 3일 | 높음 |
| 채널용 인증서 피닝 | 2일 | 중간 |
| 서명된 설정 검증 | 2일 | 중간 |
| SIEM 호환 감사 내보내기 | 2일 | 중간 |
| 보안 자가 테스트 (`zeroclaw audit --check`) | 1일 | 낮음 |

**결과물**:
- 선택적 Docker 기반 실행 격리
- 채널 webhook용 HTTPS 인증서 피닝
- 설정 파일 서명 검증
- 외부 분석을 위한 JSON/CSV 감사 내보내기

---

## 새 설정 스키마 미리보기

```toml
[security]
level = "strict"  # relaxed | default | strict | paranoid

# Sandbox 설정
[security.sandbox]
enabled = true
backend = "auto"  # auto | firejail | bubblewrap | landlock | docker | none

# 리소스 제한
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

# 자율성 (기존, 향상됨)
[autonomy]
level = "supervised"  # readonly | supervised | full
allowed_commands = ["git", "ls", "cat", "grep", "find"]
forbidden_paths = ["/etc", "/root", "~/.ssh"]
require_approval_for_medium_risk = true
block_high_risk_commands = true
max_actions_per_hour = 20
```

---

## CLI 명령어 미리보기

```bash
# 보안 상태 확인
zeroclaw security --check
# → ✓ Sandbox: Firejail active
# → ✓ Audit logging enabled (42 events today)
# → → Resource limits: 512MB mem, 50% CPU

# Audit log 쿼리
zeroclaw audit --user @alice --since 24h
zeroclaw audit --risk high --violations-only
zeroclaw audit --verify-signatures

# Sandbox 테스트
zeroclaw sandbox --test
# → Testing isolation...
#   ✓ Cannot read /etc/passwd
#   ✓ Cannot access ~/.ssh
#   ✓ Can read /workspace
```

---

## 요약

**ZeroClaw는 이미 PicoClaw보다 더 안전합니다**:
- 50% 더 작은 바이너리 (3.4MB vs 8MB)
- 50% 더 적은 RAM (< 5MB vs < 10MB)
- 100배 더 빠른 시작 (< 10ms vs < 1s)
- 포괄적인 보안 정책 엔진
- 광범위한 테스트 커버리지

**이 로드맵을 구현하면** ZeroClaw는 다음과 같이 변합니다:
- OS 레벨 sandbox로 프로덕션 등급
- 메모리/CPU 가드로 리소스 인지
- 변조 방지 로깅으로 감사 대비
- 구성 가능한 보안 수준으로 엔터프라이즈 대비

**예상 노력**: 전체 구현에 4-7주
**가치**: ZeroClaw를 "테스트에 안전한"에서 "프로덕션에 안전한"으로 전환
