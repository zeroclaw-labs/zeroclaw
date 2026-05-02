# 멀티 플랫폼 빌드 가이드 — Windows / macOS / Android / iOS

> 영문 정본: [`multi-platform-build-guide.md`](multi-platform-build-guide.md). 이 문서는 한국어 사용자용 사이드 사본입니다. 본문 변경은 영문본을 우선 갱신한 뒤 이 파일을 동기화하세요.

MoA + ZeroClaw를 4개 플랫폼(Windows / macOS / Android / iOS)에서 직접 빌드해 본인 디바이스에 설치하고, 디바이스 간 메모리 동기화와 SLM(Gemma 계열) 동작을 검증하기 위한 매뉴얼입니다.

## 이 문서를 읽어야 하는 사람

다음을 모두 직접 하려는 메인테이너 / 파워 유저용입니다:

- Windows 데스크톱 + macOS 노트북에 MoA를 "프로덕션 스타일" 앱으로 설치
- Android 폰 + iOS 시뮬레이터에 MoA를 설치해서 디바이스 간 동기화 테스트
- 각 디바이스에서 Ollama로 SLM(Gemma 계열)을 띄우고, 퍼스트브레인 / 세컨드브레인 메모리가 디바이스 간에 동기화되는지 확인

태그 릴리스나 CI 기반 배포 아티팩트가 목적이면 이 문서가 아닌 [`../release-process.md`](../release-process.md)를 참조하세요.

## 빌드 모드 정책

| 플랫폼 | 빌드 모드 | 이유 |
|--------|-----------|------|
| Windows (Tauri) | `release` | 한 번 빌드해서 설치하면 끝, 초기 컴파일이 길어도 감당 가능 |
| macOS (Tauri) | `release` | 동일 |
| Android | `debug` | 반복 변경이 많고 sideload, 서명 불필요 |
| iOS Simulator | `debug` | 시뮬레이터는 시뮬레이터 타깃 빌드만 받음, 서명 불필요 |

각 빌드 스크립트는 두 모드 모두 지원합니다. 모드를 바꾸려면 해당 섹션의 옵션 참고.

---

## 1) Windows — Tauri release + MSI/NSIS 인스톨러

Windows에서 "프로덕션 설치"를 만드는 정식 경로입니다.

### 1.1 사전 요구사항

PowerShell에서 확인 / 설치합니다 (관리자 권한은 표시한 항목만 필요):

| 요구사항 | 확인 명령 | 설치 방법 |
|----------|-----------|-----------|
| Rust 툴체인 (1.81+) | `cargo --version` | `winget install Rustlang.Rustup` 후 `rustup default stable` |
| Node.js (LTS) | `node --version` | `winget install OpenJS.NodeJS.LTS` |
| MSVC C++ 빌드 도구 | "Visual Studio Installer"에 "C++ 데스크톱 개발" 워크로드 표시 | [Build Tools for Visual Studio](https://visualstudio.microsoft.com/downloads/) 설치 → **"C++ 데스크톱 개발"** 워크로드 체크 (관리자) |
| WebView2 런타임 | Windows 11에는 기본 포함, 10이면 [Evergreen 런타임](https://developer.microsoft.com/microsoft-edge/webview2/) 설치 | 원샷 인스톨러 |
| Git Bash | `bash --version` | Git for Windows에 포함 |

> Windows의 Tauri는 **MSVC** 툴체인을 사용합니다. `rustup show`로 활성 툴체인이 `stable-x86_64-pc-windows-msvc`인지 확인하세요. GNU 툴체인은 지원하지 않습니다.

### 1.2 빌드

레포 루트에서 **Git Bash**로 실행 (스크립트가 POSIX 셸 문법):

```bash
bash scripts/build-tauri.sh
```

스크립트가 하는 일:

1. `cargo build --release` → `target/release/zeroclaw.exe` 생성
2. 그 바이너리를 `clients/tauri/src-tauri/binaries/zeroclaw-<host-triple>.exe`로 복사 (Tauri 사이드카)
3. 첫 실행 시 `clients/tauri/`에서 `npm install`
4. `npx tauri build` → 인스톨러 번들 생성

예상 시간: 첫 빌드 30~40분, 증분 빌드 3~6분.

### 1.3 산출물

빌드 성공 후 위치:

```
clients/tauri/src-tauri/target/release/bundle/
├── msi/MoA - Master of AI_0.1.0_x64_en-US.msi
└── nsis/MoA - Master of AI_0.1.0_x64-setup.exe
```

둘 중 하나만 쓰면 됩니다. 관리형 환경(회사 PC)에는 MSI, 개인용에는 NSIS가 더 가볍고 빠릅니다.

### 1.4 설치

MSI나 `-setup.exe`를 더블클릭. `C:\Program Files\MoA - Master of AI\`에 설치되고 시작 메뉴에 등록됩니다. 제거는 설정 → 앱.

### 1.5 디버그 빌드

```bash
bash scripts/build-tauri.sh --debug
```

`clients/tauri/src-tauri/target/debug/bundle/`에 디버그 번들 생성. 컴파일이 더 빠르고 패닉 추적이 풍부합니다.

---

## 2) macOS — Tauri release + .dmg

Windows에서 macOS 번들 크로스 컴파일은 **불가능**합니다 (코드 사이닝 + Apple SDK 제약). 이 단계는 반드시 맥에서 직접 실행하세요.

### 2.1 사전 요구사항 (맥에서)

```bash
# Xcode 커맨드라인 도구
xcode-select --install

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Node.js (Homebrew 경유)
brew install node
```

### 2.2 클론 + 빌드

```bash
git clone https://github.com/Kimjaechol/MoA_new.git
cd MoA_new
bash scripts/build-tauri.sh
```

호스트 트리플은 자동 감지:

- Apple Silicon → `aarch64-apple-darwin`
- Intel → `x86_64-apple-darwin`

두 아키텍처 모두에서 도는 **유니버설 바이너리**를 만들려면:

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
bash scripts/build-tauri.sh --target universal-apple-darwin
```

### 2.3 산출물 + 설치

```
clients/tauri/src-tauri/target/release/bundle/dmg/MoA - Master of AI_0.1.0_*.dmg
```

`.dmg`를 열고 MoA 아이콘을 Applications 폴더로 드래그.

> **첫 실행 시:** Apple 노타라이제이션이 안 된 빌드라 Gatekeeper가 막습니다. **시스템 설정 → 개인정보 보호 및 보안 → "그래도 열기"** 클릭하거나, 한 번만 다음 명령 실행:
>
> ```bash
> xattr -dr com.apple.quarantine "/Applications/MoA - Master of AI.app"
> ```

### 2.4 업데이트 / 제거

`macos-update-uninstall.md` 문서의 표준 절차 참고: [`macos-update-uninstall.md`](macos-update-uninstall.md) (영문).

---

## 3) Android — debug 바이너리 또는 debug APK

이 레포는 **두 가지 Android 경로**를 지원합니다. 테스트 목적에 따라 **하나만** 고르세요.

| 경로 | 명령 표면 | UI | 언제 |
|------|-----------|----|------|
| **A. Termux + ZeroClaw 바이너리** | CLI만 (데스크톱과 동일한 `zeroclaw`) | 없음 | 메모리 동기화 + SLM 테스트 — 가장 빠른 반복 |
| **B. 네이티브 MoA Android 앱 (`clients/android/`)** | 임베디드 ZeroClaw + 네이티브 UI | 풀 Android UI | UI 도그푸딩, 실사용자 시나리오 |

이 가이드의 디바이스 간 메모리 + SLM 테스트 시나리오에는 **A부터 시작하는 것이 효율적**입니다. Android UI까지 검증하려는 시점에서 B로 넘어가세요.

### 3.1 경로 A — Termux + 크로스 컴파일된 ZeroClaw 바이너리

Windows에서 (또는 macOS/Linux에서) 크로스 컴파일 → Termux로 전송 → 실행.

#### 3.1.1 Windows에서 크로스 컴파일

```bash
# 일회성 셋업
rustup target add aarch64-linux-android   # 64비트 Android (요즘 폰 거의 다)
cargo install cross                        # NDK 자동 처리

# 빌드
cross build --release --target aarch64-linux-android
# 산출물: target/aarch64-linux-android/release/zeroclaw
```

32비트 폰이면 `armv7-linux-androideabi`로 대체.

> `cross`는 NDK 툴체인을 컨테이너 안에서 돌리므로 `ANDROID_NDK_HOME`을 직접 설정할 필요가 없습니다. **Windows에서는 Docker Desktop이 실행 중**이어야 `cross`가 작동합니다.

#### 3.1.2 폰에 Termux 설치

**F-Droid**에서 Termux 설치 (Play Store 버전은 오래되어 지원 안 됨 — [`../android-setup.md`](../android-setup.md)의 경고 참조).

#### 3.1.3 전송 + 실행

```bash
# Termux에서
termux-setup-storage
# USB 또는 클라우드로 zeroclaw 바이너리를 ~/storage/shared 에 옮긴 뒤:
cp ~/storage/shared/zeroclaw $PREFIX/bin/
chmod +x $PREFIX/bin/zeroclaw
zeroclaw --version
zeroclaw onboard
```

참조: [`../android-setup.md`](../android-setup.md) (영문) — Termux 세부, `termux-services`로 데몬화, Termux 안에서 직접 빌드하는 경로 등.

### 3.2 경로 B — 네이티브 MoA Android 앱 (APK)

#### 3.2.1 사전 요구사항

- Android Studio + Android SDK (API 34 권장)
- Java 17 (Android Studio에 동봉)
- CLI 빌드만 할 거면 `ANDROID_HOME` 환경 변수 설정

#### 3.2.2 빌드

Android Studio에서 `clients/android` 폴더 열기 → **Build → Build Bundle(s) / APK(s) → Build APK(s)**. 또는 CLI:

```bash
cd clients/android
./gradlew assembleDebug
```

산출물:

```
clients/android/app/build/outputs/apk/debug/app-debug.apk
```

#### 3.2.3 폰에 설치

```bash
# 폰에서 USB 디버깅 활성화 (설정 → 휴대전화 정보 → 빌드 번호 7번 탭 → 개발자 옵션 → USB 디버깅)
adb install clients/android/app/build/outputs/apk/debug/app-debug.apk
```

또는 APK를 폰으로 옮긴 뒤 직접 탭해서 설치 (파일 관리자 앱에 "출처를 알 수 없는 앱 설치" 권한 필요).

---

## 4) iOS Simulator — debug 빌드

이 단계는 **Xcode 15+가 설치된 맥**이 필수입니다. Windows에서는 iOS 타깃 빌드 자체가 불가능합니다.

### 4.1 사전 요구사항 (맥에서)

```bash
# Xcode (App Store에서 먼저 설치)
xcode-select --install

# 시뮬레이터 타깃
rustup target add aarch64-apple-ios-sim

# 실기기에 깔 거면
rustup target add aarch64-apple-ios
```

### 4.2 정적 라이브러리 + Xcode 프로젝트 빌드

레포에 `scripts/build-ios.sh` 스크립트가 있습니다:

```bash
# 기본 = 시뮬레이터 debug
bash scripts/build-ios.sh

# 라이브러리만 (Xcode 단계 스킵)
bash scripts/build-ios.sh lib-only

# 실기기용 archive (release)
bash scripts/build-ios.sh release
```

시뮬레이터-debug 경로가 하는 일:

1. `clients/ios-bridge/`에서 `cargo build --target aarch64-apple-ios-sim`
2. `libzeroclaw_ios.a` 생성
3. `clients/ios/MoA.xcodeproj`을 iPhone 16 시뮬레이터 타깃으로 빌드

### 4.3 시뮬레이터에서 실행

가장 쉬운 방법: Xcode에서 `clients/ios/MoA.xcodeproj` 열고, 상단 디바이스 셀렉터에서 시뮬레이터 골라서 **▶ Run** (`Cmd + R`).

CLI로 부팅된 시뮬레이터에 설치:

```bash
xcrun simctl install booted clients/ios/build/Debug-iphonesimulator/MoA.app
xcrun simctl launch booted com.moa.agent
```

### 4.4 실기기 설치 (release)

`bash scripts/build-ios.sh release`가 `.xcarchive`를 만듭니다. 다음으로 `.ipa` 추출:

```bash
xcodebuild -exportArchive \
  -archivePath clients/ios/build/MoA.xcarchive \
  -exportPath clients/ios/build/ \
  -exportOptionsPlist clients/ios/ExportOptions.plist
```

실기기 sideload는 유료 Apple Developer 계정 + 프로비저닝 프로파일이 필요합니다. 개인 Apple ID로 7일짜리 무료 사이닝은 Xcode → Devices & Simulators 흐름으로 가능.

---

## 5) 디바이스 간 테스트 시나리오 — 메모리 동기화 + SLM

4개 중 최소 2개 플랫폼이 설치되면 디바이스 간 동작을 검증할 수 있습니다.

### 5.1 각 디바이스에 Ollama 설치 + SLM pull

`ollama search gemma`로 정확한 태그를 먼저 확인하세요. Gemma 시리즈는 자주 갱신되며, 아래 태그는 **예시**입니다 (그대로 박제된 게 아닙니다).

```bash
# Windows / macOS
# https://ollama.com 에서 인스톨러 다운로드 후 설치
ollama pull gemma3:4b   # 테스트할 Gemma 변종에 맞춰 조정
ollama pull gemma2:2b   # 폰용 작은 모델
```

Android Termux는 RAM 한계 때문에 4B급은 보통 무겁습니다. 작은 변종을 우선 시도하고, 자세한 폰용 Ollama 셋업은 [`../android-setup.md`](../android-setup.md) (영문) 참조.

### 5.2 동기화 백엔드 먼저 확인

"퍼스트브레인 / 세컨드브레인" 페어는 이 레포의 **vault 모듈**이 담당합니다. 디바이스 간 테스트 계획을 짜기 **전에** 어떤 동기화 채널을 쓰는지 식별해야 합니다:

- 로컬 SQLite만 쓰고 동기화 없음
- 클라우드 마운트 폴더 경유 파일시스템 동기화 (iCloud Drive, Nextcloud 등)
- 네트워크 기반 동기화 (R2/S3, P2P, 자체 서버)

채널이 무엇이냐에 따라 **어떤 디바이스끼리 서로 보이는지**, **각 디바이스에 어떤 자격 증명이 필요한지**, **어떤 실패 모드를 살펴봐야 하는지**가 달라집니다. 정답은 `src/vault/` 코드와 `docs/reference/` 안에 있습니다.

> 이 부분은 다음 단계 작업 항목입니다 — 빌드 가이드 PR 머지 후 `src/vault/` 분석으로 이어집니다.

### 5.3 권장 스모크 테스트

1. **디바이스 A (Windows)** — MoA 켜고 메모 작성: *"내일 오후 3시에 Q2 회고 발표"*
2. 동기화 대기 (또는 동기화 명령이 있다면 수동 트리거)
3. **디바이스 B (Android Termux)** — `zeroclaw memory list`로 같은 메모가 보이는지 확인
4. **디바이스 C (iOS Simulator)** — MoA 앱에서 로컬 Gemma에 질문: *"내일 일정이 뭐였지?"* → A에서 작성한 메모를 인용해 답하는지 확인
5. 디바이스 B에서 메모를 수정 → A와 C에 반영되는지 확인
6. 디바이스 B의 네트워크를 끊고 A에서 수정 → B를 다시 연결 → 충돌 해결 동작이 기대대로인지 확인

---

## 6) 처음 시작할 때 권장 순서

제로에서 시작해 가장 짧은 시간에 테스트까지 가려면:

1. **Windows Tauri release를 먼저 시작** — 빌드 시간이 가장 길어 다른 작업과 병렬화 가능
2. 컴파일 도는 동안 **Android 경로 A vs B 결정** + Termux 또는 Android Studio 설치
3. Windows 설치 끝나면 → Ollama + Gemma 받고 단일 디바이스 정상 동작 확인
4. **Android 경로 A** — 두 번째 디바이스를 가장 빨리 확보, 실제 디바이스 간 동기화 테스트 가능
5. **macOS** — 맥 앞에 가게 됐을 때, 같은 `bash scripts/build-tauri.sh` 흐름
6. **iOS Simulator** — 마지막. iOS UI 표면 확인용이며, 새 동기화 채널을 추가하지는 않음

---

## 7) 트러블슈팅 힌트

| 증상 | 가능한 원인 | 어디 보기 |
|------|-------------|-----------|
| Windows에서 `link.exe not found` | MSVC C++ 빌드 도구 누락 | 1.1 |
| `tauri: command not found` | `clients/tauri/`에서 `npm install` 안 함 | 스크립트가 자동으로 하지만 cargo 직접 돌렸다면 한 번 `npm install` |
| macOS에서 DMG 단계가 빠짐 | `create-dmg` 미설치 | `brew install create-dmg` 후 재실행 |
| Windows에서 `cross` 실패 | Docker Desktop 미실행 | Docker Desktop 켜고 재시도 |
| Android APK 설치는 되는데 실행 즉시 크래시 | 디바이스와 네이티브 lib ABI 불일치 | `clients/android/app/src/main/jniLibs/` 의 ABI 폴더 확인 |
| iOS Simulator가 빈 화면 | 시뮬레이터가 부팅 완료 전에 install | `xcrun simctl bootstatus booted -b` 후 재시도 |
| Gemma 응답이 동기화된 메모를 무시 | 동기화 미실행, 또는 vault 채널 불일치 | 5.2 — 채널부터 검증 |

레포 전반 트러블슈팅은 [`../troubleshooting.md`](../troubleshooting.md) (영문).

---

## 8) 관련 문서

- [`../android-setup.md`](../android-setup.md) — Termux + Android 온디바이스 빌드 상세 (영문)
- [`../one-click-bootstrap.md`](../one-click-bootstrap.md) — 빌드 외 운영자 셋업 (영문)
- [`macos-update-uninstall.md`](macos-update-uninstall.md) — macOS 라이프사이클 (영문)
- [`../release-process.md`](../release-process.md) — 태그 릴리스 워크플로 (이 가이드는 개인/개발 설치용이며 릴리스용이 아님) (영문)
- [`../operations/README.md`](../operations/README.md) — 설치 후 런타임 운영 (영문)
