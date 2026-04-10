# Android 설정

ZeroClaw는 Android 기기용 사전 빌드 바이너리를 제공합니다.

## 지원 아키텍처

| 타겟 | Android 버전 | 기기 |
|--------|-----------------|---------|
| `armv7-linux-androideabi` | Android 4.1+ (API 16+) | 구형 32비트 폰 (Galaxy S3 등) |
| `aarch64-linux-android` | Android 5.0+ (API 21+) | 최신 64비트 폰 |

## Termux를 통한 설치

Android에서 ZeroClaw를 실행하는 가장 쉬운 방법은 [Termux](https://termux.dev/)를 사용하는 것입니다.

### 1. Termux 설치

[F-Droid](https://f-droid.org/packages/com.termux/) (권장) 또는 GitHub 릴리즈에서 다운로드합니다.

> ⚠️ **참고:** Play Store 버전은 오래되었으며 지원되지 않습니다.

### 2. ZeroClaw 다운로드

```bash
# 아키텍처 확인
uname -m
# aarch64 = 64비트, armv7l/armv8l = 32비트

# 적절한 바이너리 다운로드
# 64비트 (aarch64)의 경우:
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-linux-android.tar.gz
tar xzf zeroclaw-aarch64-linux-android.tar.gz

# 32비트 (armv7)의 경우:
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-linux-androideabi.tar.gz
tar xzf zeroclaw-armv7-linux-androideabi.tar.gz
```

### 3. 설치 및 실행

```bash
chmod +x zeroclaw
mv zeroclaw $PREFIX/bin/

# 설치 확인
zeroclaw --version

# 설정 실행
zeroclaw onboard
```

## ADB를 통한 직접 설치

Termux 외부에서 ZeroClaw를 실행하려는 고급 사용자를 위한 방법입니다:

```bash
# ADB가 설치된 컴퓨터에서
adb push zeroclaw /data/local/tmp/
adb shell chmod +x /data/local/tmp/zeroclaw
adb shell /data/local/tmp/zeroclaw --version
```

> ⚠️ Termux 외부에서 실행하려면 전체 기능을 사용하기 위해 루팅된 기기 또는 특정 권한이 필요합니다.

## Android에서의 제한 사항

- **systemd 없음:** 데몬 모드에는 Termux의 `termux-services`를 사용합니다
- **저장소 접근:** Termux 저장소 권한이 필요합니다 (`termux-setup-storage`)
- **네트워크:** 일부 기능은 로컬 바인딩을 위해 Android VPN 권한이 필요할 수 있습니다

## 소스에서 빌드

Android용으로 직접 빌드하려면:

```bash
# Android NDK 설치
# 타겟 추가
rustup target add armv7-linux-androideabi aarch64-linux-android

# NDK 경로 설정
export ANDROID_NDK_HOME=/path/to/ndk
export PATH=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH

# 빌드
cargo build --release --target armv7-linux-androideabi
cargo build --release --target aarch64-linux-android
```

## 문제 해결

### "Permission denied"
```bash
chmod +x zeroclaw
```

### "not found" 또는 링커 오류
기기에 맞는 올바른 아키텍처를 다운로드했는지 확인합니다.

### 구형 Android (4.x)
API 레벨 16+의 `armv7-linux-androideabi` 빌드를 사용합니다.
