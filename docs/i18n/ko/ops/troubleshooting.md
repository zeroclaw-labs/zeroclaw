# ZeroClaw 문제 해결

이 가이드는 일반적인 설치/런타임 장애와 빠른 해결 방법에 초점을 맞추고 있습니다.

최종 검증일: **2026년 2월 20일**.

## 설치 / 부트스트랩

### `cargo` not found

증상:

- 부트스트랩이 `cargo is not installed` 오류와 함께 종료

해결:

```bash
./install.sh --install-rust
```

또는 <https://rustup.rs/>에서 설치하십시오.

### 시스템 빌드 의존성 누락

증상:

- 컴파일러 또는 `pkg-config` 관련 문제로 빌드 실패

해결:

```bash
./install.sh --install-system-deps
```

### 저사양 RAM / 저용량 디스크 호스트에서 빌드 실패

증상:

- `cargo build --release`가 종료됨 (`signal: 9`, OOM killer 또는 `cannot allocate memory`)
- swap 추가 후에도 디스크 용량 부족으로 빌드 실패

원인:

- 런타임 메모리(일반 작업 시 5MB 미만)는 컴파일 시 메모리와 다릅니다.
- 전체 소스 빌드에는 **2GB RAM + swap** 및 **6GB 이상의 여유 디스크 공간**이 필요할 수 있습니다.
- 작은 디스크에서 swap을 활성화하면 RAM OOM은 피할 수 있지만 디스크 고갈로 여전히 실패할 수 있습니다.

제한된 머신에서 권장하는 방법:

```bash
./install.sh --prefer-prebuilt
```

바이너리 전용 모드 (소스 빌드 없음):

```bash
./install.sh --prebuilt-only
```

제한된 호스트에서 반드시 소스 컴파일이 필요한 경우:

1. swap과 빌드 출력 모두를 위한 충분한 여유 디스크가 있는 경우에만 swap을 추가하십시오.
1. cargo 병렬 처리를 제한하십시오:

```bash
CARGO_BUILD_JOBS=1 cargo build --release --locked
```

1. Matrix가 필요하지 않은 경우 무거운 feature를 줄이십시오:

```bash
cargo build --release --locked --features hardware
```

1. 더 강력한 머신에서 크로스 컴파일하여 바이너리를 대상 호스트로 복사하십시오.

### 빌드가 매우 느리거나 멈춘 것처럼 보이는 경우

증상:

- `cargo check` / `cargo build`가 `Checking zeroclaw`에서 오랫동안 멈춘 것처럼 보임
- `Blocking waiting for file lock on package cache` 또는 `build directory` 반복 발생

ZeroClaw에서 이런 현상이 발생하는 이유:

- Matrix E2EE 스택 (`matrix-sdk`, `ruma`, `vodozemac`)이 크고 타입 체크 비용이 높습니다.
- TLS + 암호화 네이티브 빌드 스크립트 (`aws-lc-sys`, `ring`)가 상당한 컴파일 시간을 추가합니다.
- `rusqlite`는 번들된 SQLite를 로컬에서 C 코드로 컴파일합니다.
- 여러 cargo 작업/worktree를 동시에 실행하면 lock 경합이 발생합니다.

빠른 확인:

```bash
cargo check --timings
cargo tree -d
```

타이밍 리포트는 `target/cargo-timings/cargo-timing.html`에 생성됩니다.

더 빠른 로컬 반복 (Matrix 채널이 필요하지 않은 경우):

```bash
cargo check
```

기본 기능 세트만 사용하여 컴파일 시간을 크게 줄일 수 있습니다.

Matrix 지원을 명시적으로 활성화하여 빌드하려면:

```bash
cargo check --features channel-matrix
```

Matrix + Lark + hardware 지원으로 빌드하려면:

```bash
cargo check --features hardware,channel-matrix,channel-lark
```

Lock 경합 완화:

```bash
pgrep -af "cargo (check|build|test)|cargo check|cargo build|cargo test"
```

빌드 실행 전에 관련 없는 cargo 작업을 중지하십시오.

### 설치 후 `zeroclaw` 명령어를 찾을 수 없는 경우

증상:

- 설치는 성공했지만 셸에서 `zeroclaw`를 찾을 수 없음

해결:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
which zeroclaw
```

필요한 경우 셸 프로필에 영구 등록하십시오.

## 런타임 / Gateway

### Gateway에 접근할 수 없는 경우

확인 사항:

```bash
zeroclaw status
zeroclaw doctor
```

`~/.zeroclaw/config.toml` 확인:

- `[gateway].host` (기본값 `127.0.0.1`)
- `[gateway].port` (기본값 `42617`)
- `allow_public_bind`는 LAN/공용 인터페이스를 의도적으로 노출할 때만 사용

### 페어링 / 인증 실패 (webhook)

확인 사항:

1. 페어링이 완료되었는지 확인 (`/pair` 플로우)
2. bearer token이 유효한지 확인
3. 진단 재실행:

```bash
zeroclaw doctor
```

## 채널 관련 문제

### Telegram 충돌: `terminated by other getUpdates request`

원인:

- 동일한 봇 토큰을 사용하는 여러 폴러가 존재

해결:

- 해당 토큰에 대해 하나의 활성 런타임만 유지
- 추가 `zeroclaw daemon` / `zeroclaw channel start` 프로세스를 중지

### `channel doctor`에서 채널이 비정상으로 표시되는 경우

확인 사항:

```bash
zeroclaw channel doctor
```

그런 다음 설정에서 채널별 자격 증명 및 허용 목록 필드를 확인하십시오.

## 서비스 모드

### 서비스가 설치되었지만 실행되지 않는 경우

확인 사항:

```bash
zeroclaw service status
```

복구:

```bash
zeroclaw service stop
zeroclaw service start
```

Linux 로그:

```bash
journalctl --user -u zeroclaw.service -f
```

## 설치 URL

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

## 여전히 해결되지 않는 경우

이슈를 등록할 때 다음 출력을 수집하여 포함하십시오:

```bash
zeroclaw --version
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

OS, 설치 방법, 그리고 민감 정보가 제거된 설정 스니펫도 함께 포함하십시오.

## 관련 문서

- [operations-runbook.md](operations-runbook.md)
- [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)
- [channels-reference.md](../reference/api/channels-reference.md)
- [network-deployment.md](network-deployment.md)
