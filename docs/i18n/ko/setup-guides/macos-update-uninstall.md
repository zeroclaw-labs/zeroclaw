# macOS 업데이트 및 제거 가이드

이 페이지는 macOS (OS X)에서 ZeroClaw의 지원되는 업데이트 및 제거 절차를 설명합니다.

최종 검증일: **2026년 2월 22일**.

## 1) 현재 설치 방법 확인

```bash
which zeroclaw
zeroclaw --version
```

일반적인 위치:

- Homebrew: `/opt/homebrew/bin/zeroclaw` (Apple Silicon) 또는 `/usr/local/bin/zeroclaw` (Intel)
- Cargo/bootstrap/수동: `~/.cargo/bin/zeroclaw`

둘 다 존재하는 경우, 셸의 `PATH` 순서에 따라 어느 것이 실행되는지 결정됩니다.

## 2) macOS에서 업데이트

### A) Homebrew 설치

```bash
brew update
brew upgrade zeroclaw
zeroclaw --version
```

### B) Clone + bootstrap 설치

로컬 저장소 체크아웃에서:

```bash
git pull --ff-only
./install.sh --prefer-prebuilt
zeroclaw --version
```

소스 전용 업데이트를 원하는 경우:

```bash
git pull --ff-only
cargo install --path . --force --locked
zeroclaw --version
```

### C) 수동 사전 빌드 바이너리 설치

최신 릴리스 에셋으로 다운로드/설치 흐름을 다시 실행한 후 확인합니다:

```bash
zeroclaw --version
```

## 3) macOS에서 제거

### A) 먼저 백그라운드 서비스 중지 및 제거

바이너리 제거 후 데몬이 계속 실행되는 것을 방지합니다.

```bash
zeroclaw service stop || true
zeroclaw service uninstall || true
```

`service uninstall`로 제거되는 서비스 아티팩트:

- `~/Library/LaunchAgents/com.zeroclaw.daemon.plist`

### B) 설치 방법별 바이너리 제거

Homebrew:

```bash
brew uninstall zeroclaw
```

Cargo/bootstrap/수동 (`~/.cargo/bin/zeroclaw`):

```bash
cargo uninstall zeroclaw || true
rm -f ~/.cargo/bin/zeroclaw
```

### C) 선택사항: 로컬 런타임 데이터 제거

config, 인증 프로필, 로그 및 workspace 상태를 완전히 정리하려는 경우에만 실행하세요.

```bash
rm -rf ~/.zeroclaw
```

## 4) 제거 완료 확인

```bash
command -v zeroclaw || echo "zeroclaw binary not found"
pgrep -fl zeroclaw || echo "No running zeroclaw process"
```

`pgrep`에서 여전히 프로세스가 발견되면, 수동으로 중지한 후 다시 확인하세요:

```bash
pkill -f zeroclaw
```

## 관련 문서

- [원클릭 부트스트랩](one-click-bootstrap.md)
- [명령어 참조](../reference/cli/commands-reference.md)
- [문제 해결](../ops/troubleshooting.md)
