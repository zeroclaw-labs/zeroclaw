# main.rs 리팩토링 계획서

**날짜:** 2026-02-28
**담당:** core-architect
**우선순위:** 중간

## 1. 현황 분석

### 1.1 현재 main.rs 구조 (1689줄)

| 구성 요소 | 라인 범위 | 줄 수 | 설명 |
|-----------|-----------|-------|------|
| Clippy lint 설정 | 1-33 | 33 | Clippy 경고/허용 설정 |
| Imports | 35-93 | 59 | 모듈 import, re-export |
| CompletionShell enum | 95-107 | 13 | 쉘 완성 지원 |
| Cli struct | 109-121 | 13 | CLI 메인 구조체 |
| Commands enum | 123-437 | ~315 | 메인 명령어 enum |
| ConfigCommands enum | 439-443 | 5 | 설정 관리 하위 명령 |
| AuthCommands enum | 445-526 | 82 | 인증 관리 하위 명령 |
| ModelCommands enum | 528-540 | 13 | 모델 관리 하위 명령 |
| DoctorCommands enum | 542-569 | 28 | 진단 하위 명령 |
| MemoryCommands enum | 571-599 | 29 | 메모리 관리 하위 명령 |
| main() 함수 | 601-951 | ~351 | 메인 진입점 |
| PendingOAuthLogin structs | 973-998 | 26 | OAuth 로그인 상태 |
| OAuth helper functions | 1000-1098 | ~99 | OAuth 보조 함수 |
| handle_auth_command() | 1131-1590 | ~460 | 인증 명령 핸들러 |
| write_shell_completion() | 953-971 | 19 | 쉘 완성 생성 |
| Tests | 1592-1690 | ~99 | 단위 테스트 |

### 1.2 문제점

1. **과도한 복잡성:** 단일 파일에 너무 많은 책임 (CLI 정의 + 핸들러 + OAuth 로직)
2. **유지보수 어려움:** 관련 없는 코드가 섞여 있어 찾기 어려움
3. **테스트 어려움:** 큰 파일 테스트 시 오버헤드
4. **코드 재사용성 낮음:** OAuth 로직 등을 다른 곳에서 재사용 불가

## 2. 리팩토링 원칙

### 2.1 아키텍처 원칙

1. **KISS (Keep It Simple, Stupid):**
   - 단순한 파일 분리, 복잡한 추상화 계층 금지
   - 새로운 trait나 구조체 추가 최소화

2. **YAGNI (You Aren't Gonna Need It):**
   - 명확한 필요 없는 분리 지양
   - 미래의 "가능성"을 위한 추상화 금지

3. **단일 책임 원칙 (SRP):**
   - 각 모듈은 하나의 명령어 그룹만 담당
   - enum 정의와 핸들러를 같은 파일에 배치

4. **기존 동작 유지:**
   - CLI 인터페이스 변경 없음
   - 사용자 관점 동일하게 유지

### 2.2 파일 명명 규칙

```
src/cli/
├── mod.rs           # CLI 모듈 진입점
├── auth.rs          # AuthCommands + 핸들러
├── config.rs        # ConfigCommands + 핸들러
├── models.rs        # ModelCommands + 핸들러
├── doctor.rs        # DoctorCommands + 핸들러
├── memory.rs        # MemoryCommands + 핸들러
└── completions.rs   # CompletionShell + shell 완성
```

## 3. 상세 리팩토링 계획

### 3.1 1단계: completions 분리 (가장 단순, 낮은 위험)

**파일:** `src/cli/completions.rs`

**이동 항목:**
- `CompletionShell` enum
- `write_shell_completion()` 함수

**의존성:**
- `clap_complete::{generate, shells}`
- `std::io::Write`
- `Cli` struct (main.rs 참조 필요)

**구현:**
```rust
// src/cli/completions.rs
use clap::{CommandParser, ValueEnum};
use clap_complete::{generate, shells};
use std::io::Write;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CompletionShell {
    #[value(name = "bash")]
    Bash,
    #[value(name = "fish")]
    Fish,
    #[value(name = "zsh")]
    Zsh,
    #[value(name = "powershell")]
    PowerShell,
    #[value(name = "elvish")]
    Elvish,
}

pub fn write_shell_completion<W: Write>(
    shell: CompletionShell,
    writer: &mut W,
) -> Result<()> {
    // 구현...
}
```

### 3.2 2단계: ConfigCommands 분리

**파일:** `src/cli/config.rs`

**이동 항목:**
- `ConfigCommands` enum

**핸들러 위치:** main.rs의 match arm 유지 (단순히 schema만 출력)

**구현:**
```rust
// src/cli/config.rs
#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Dump the full configuration JSON Schema to stdout
    Schema,
}
```

### 3.3 3단계: ModelCommands 분리

**파일:** `src/cli/models.rs`

**이동 항목:**
- `ModelCommands` enum

**핸들러:** 이미 `onboard::run_models_refresh()`로 위임되어 있으므로 enum만 분리

### 3.4 4단계: DoctorCommands 분리

**파일:** `src/cli/doctor.rs`

**이동 항목:**
- `DoctorCommands` enum

**핸들러:** 이미 `doctor::run_models()`, `doctor::run_traces()`, `doctor::run()`로 위임되어 있으므로 enum만 분리

### 3.5 5단계: MemoryCommands 분리

**파일:** `src/cli/memory.rs`

**이동 항목:**
- `MemoryCommands` enum

**핸들러:** 이미 `memory::cli::handle_command()`로 위임되어 있으므로 enum만 분리

### 3.6 6단계: AuthCommands 분리 (가장 복잡)

**파일:** `src/cli/auth.rs`

**이동 항목:**
- `AuthCommands` enum
- `PendingOAuthLogin` struct
- `PendingOAuthLoginFile` struct
- `pending_oauth_login_path()` 함수
- `pending_oauth_secret_store()` 함수
- `set_owner_only_permissions()` 함수
- `save_pending_oauth_login()` 함수
- `load_pending_oauth_login()` 함수
- `clear_pending_oauth_login()` 함수
- `read_auth_input()` 함수
- `read_plain_input()` 함수
- `extract_openai_account_id_for_profile()` 함수
- `format_expiry()` 함수
- `handle_auth_command()` 함수

**새로운 pub 함수:**
```rust
pub async fn handle_auth_command(
    auth_command: AuthCommands,
    config: &Config
) -> Result<()>
```

### 3.7 CLI 모듈 통합

**파일:** `src/cli/mod.rs`

```rust
mod completions;
mod auth;
mod config;
mod models;
mod doctor;
mod memory;

pub use completions::{CompletionShell, write_shell_completion};
pub use auth::{AuthCommands, handle_auth_command};
pub use config::ConfigCommands;
pub use models::ModelCommands;
pub use doctor::DoctorCommands;
pub use memory::MemoryCommands;
```

## 4. 변경 후 main.rs 구조 (예상)

```
main.rs (목표: ~500줄)
├── Clippy lint 설정 (1-33)
├── Imports (35-90)              # cli 모듈 추가
├── Cli struct (92-104)
├── Commands enum (106-420)       # ServiceCommands 등만 유지
├── main() 함수 (422-650)         # 단순화된 라우팅
└── Tests (652-700)
```

## 5. 테스트 계획

### 5.1 단위 테스트

각 분리된 모듈에 대해:

1. **CLI enum 구조 검증:** clap 파싱 테스트
2. **핸들러 동작 검증:** mock으로 핵심 로직 테스트
3. **에러 경로 테스트:** 실패 시나리오 확인

### 5.2 통합 테스트

```bash
# 모든 명령어 기본 동작 확인
cargo test --test cli_integration

# 주요 명령어 수동 테스트
zeroclaw auth list
zeroclaw config schema
zeroclaw doctor models
zeroclaw memory stats
```

### 5.3 회귀 테스트

1. 기존 기능 동일 동작 확인
2. CLI 도움말 출력 확인
3. 완성 스크립트 생성 확인

## 6. 순서 및 롤백 계획

### 6.1 순서

1. completions 분리 (가장 단순)
2. config 분리
3. models 분리
4. doctor 분리
5. memory 분리
6. auth 분리 (가장 복잡)

각 단계 완료 후:
```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

### 6.2 롤백 계획

각 단계는 독립적인 커밋:
```
refactor(cli): extract completions to src/cli/completions.rs
refactor(cli): extract config commands to src/cli/config.rs
...
```

롤백 시:
```bash
git revert <commit-hash>
```

## 7. 위험 분석

### 7.1 저위험

- **completions, config, models, doctor, memory:**
  - enum만 분리
  - 핸들러는 이미 위임되어 있음
  - 변경 영향 제한적

### 7.2 중위험

- **auth:**
  - OAuth 로직이 복잡
  - 상태 관리가 파일 시스템 의존
  - 테스트 커버리지 확인 필요

### 7.3 완화 조치

1. 각 단계를 독립 커밋으로 관리
2. 단계별 `cargo test` 통과 확인
3. 브랜치에서 작업 후 PR로 검토

## 8. 산출물

1. ✅ 본 계획서
2. ⏳ `src/cli/mod.rs` 생성
3. ⏳ `src/cli/completions.rs` 생성
4. ⏳ `src/cli/config.rs` 생성
5. ⏳ `src/cli/models.rs` 생성
6. ⏳ `src/cli/doctor.rs` 생성
7. ⏳ `src/cli/memory.rs` 생성
8. ⏳ `src/cli/auth.rs` 생성
9. ⏳ main.rs 수정 (import 추가, 코드 제거)
10. ⏳ 테스트 추가/수정
11. ⏳ 문서 업데이트 (변경 사항)

## 9. 예상 결과

| 항목 | 변경 전 | 변경 후 |
|------|---------|---------|
| main.rs 줄 수 | 1689 | ~500 |
| main.rs 책임 | CLI + 핸들러 | CLI 라우팅만 |
| 파일 개수 | 1 | 8 |
| 순환 의존성 | 없음 | 없음 |
| 테스트 용이성 | 낮음 | 높음 |
