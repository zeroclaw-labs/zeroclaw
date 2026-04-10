# Windows 설정 가이드

이 가이드는 Windows에서 ZeroClaw를 빌드하고 설치하는 방법을 다룹니다.

## 빠른 시작

### 옵션 A: 원클릭 설정 스크립트

저장소 루트에서:

```cmd
setup.bat
```

스크립트가 환경을 자동 감지하고 설치 과정을 안내합니다.
대화형 메뉴를 건너뛰려면 플래그를 전달할 수도 있습니다:

| 플래그 | 설명 |
|------|-------------|
| `--prebuilt` | 사전 컴파일된 바이너리 다운로드 (가장 빠름) |
| `--minimal` | 기본 기능만으로 빌드 |
| `--standard` | Matrix + Lark/Feishu + Postgres로 빌드 |
| `--full` | 모든 기능으로 빌드 |

### 옵션 B: Scoop (패키지 관리자)

```powershell
scoop bucket add zeroclaw https://github.com/zeroclaw-labs/scoop-zeroclaw
scoop install zeroclaw
```

### 옵션 C: 수동 빌드

```cmd
rustup target add x86_64-pc-windows-msvc
cargo build --release --locked --features channel-matrix,channel-lark --target x86_64-pc-windows-msvc
copy target\x86_64-pc-windows-msvc\release\zeroclaw.exe %USERPROFILE%\.zeroclaw\bin\
```

## 사전 요구사항

| 요구사항 | 필수? | 참고 |
|-------------|-----------|-------|
| Git | 예 | [git-scm.com/download/win](https://git-scm.com/download/win) |
| Rust 1.87+ | 예 | 없으면 `setup.bat`에서 자동 설치 |
| Visual Studio Build Tools | 예 (소스 빌드) | MSVC 링커를 위해 C++ 워크로드 필요 |
| Node.js | 아니오 | 소스에서 웹 대시보드를 빌드할 때만 필요 |

### Visual Studio Build Tools 설치

Visual Studio가 설치되어 있지 않은 경우, Build Tools를 설치하세요:

1. [visualstudio.microsoft.com/visual-cpp-build-tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)에서 다운로드
2. **"Desktop development with C++"** 워크로드를 선택
3. 설치 후 터미널을 재시작

또는 Visual Studio 2019 이상에 C++ 워크로드가 설치되어 있다면, 이미 준비된 것입니다.

## Feature 플래그

ZeroClaw는 Cargo feature 플래그를 사용하여 컴파일되는 통합을 제어합니다:

| Feature | 설명 | 기본값? |
|---------|-------------|----------|
| `channel-lark` | Lark/Feishu 메시징 | 예 |
| `channel-nostr` | Nostr 프로토콜 | 예 |
| `observability-prometheus` | Prometheus 메트릭 | 예 |
| `skill-creation` | 자동 스킬 생성 | 예 |
| `channel-matrix` | Matrix 프로토콜 | 아니오 |
| `browser-native` | 헤드리스 브라우저 | 아니오 |
| `hardware` | USB 기기 지원 | 아니오 |
| `rag-pdf` | RAG를 위한 PDF 추출 | 아니오 |
| `observability-otel` | OpenTelemetry | 아니오 |

특정 feature로 빌드하려면:

```cmd
cargo build --release --locked --features channel-matrix,channel-lark --target x86_64-pc-windows-msvc
```

## 설치 후

1. **PATH 변경 사항을 적용하려면 터미널을 재시작하세요**
2. **ZeroClaw 초기화:**
   ```cmd
   zeroclaw init
   ```
3. **`%USERPROFILE%\.zeroclaw\config.toml`에서 API 키를 구성하세요**

## 문제 해결

### 링커 오류로 빌드 실패

C++ 워크로드가 포함된 Visual Studio Build Tools를 설치하세요. MSVC 링커가 필요합니다.

### `cargo build` 메모리 부족

소스 빌드에는 최소 2 GB의 여유 RAM이 필요합니다. 사전 컴파일된 바이너리를 다운로드하려면 `setup.bat --prebuilt`를 사용하세요.

### Feishu/Lark를 사용할 수 없음

Feishu와 Lark는 같은 플랫폼입니다. `channel-lark` feature로 빌드하세요:

```cmd
cargo build --release --locked --features channel-lark --target x86_64-pc-windows-msvc
```

### 웹 대시보드 누락

웹 대시보드는 빌드 시 Node.js와 npm이 필요합니다. Node.js를 설치하고 다시 빌드하거나, 대시보드가 포함된 사전 빌드 바이너리를 사용하세요.
