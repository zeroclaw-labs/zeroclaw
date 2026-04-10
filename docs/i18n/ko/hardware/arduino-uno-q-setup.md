# Arduino Uno Q에서 ZeroClaw 사용하기 — 단계별 가이드

Arduino Uno Q의 Linux 측에서 ZeroClaw를 실행합니다. Telegram은 WiFi를 통해 작동하며, GPIO 제어는 Bridge를 사용합니다 (최소한의 App Lab 앱이 필요합니다).

---

## 포함된 구성 요소 (코드 변경 불필요)

ZeroClaw에는 Arduino Uno Q에 필요한 모든 것이 포함되어 있습니다. **저장소를 클론하고 이 가이드를 따르면 됩니다 — 패치나 커스텀 코드가 필요하지 않습니다.**

| 구성 요소 | 위치 | 용도 |
|-----------|----------|---------|
| Bridge 앱 | `firmware/uno-q-bridge/` | GPIO를 위한 MCU 스케치 + Python 소켓 서버 (포트 9999) |
| Bridge 도구 | `src/peripherals/uno_q_bridge.rs` | TCP를 통해 Bridge와 통신하는 `gpio_read` / `gpio_write` 도구 |
| 설정 명령어 | `src/peripherals/uno_q_setup.rs` | `zeroclaw peripheral setup-uno-q`가 scp + arduino-app-cli를 통해 Bridge를 배포 |
| 설정 스키마 | `board = "arduino-uno-q"`, `transport = "bridge"` | `config.toml`에서 지원 |

Uno Q 지원을 포함하려면 `--features hardware`로 빌드합니다.

---

## 사전 요구사항

- Wi-Fi가 설정된 Arduino Uno Q
- Mac에 설치된 Arduino App Lab (초기 설정 및 배포용)
- LLM용 API 키 (OpenRouter 등)

---

## 1단계: 초기 Uno Q 설정 (1회만)

### 1.1 App Lab을 통한 Uno Q 설정

1. [Arduino App Lab](https://docs.arduino.cc/software/app-lab/)을 다운로드합니다 (Linux에서는 tar.gz).
2. Uno Q를 USB로 연결하고 전원을 켭니다.
3. App Lab을 열고 board에 연결합니다.
4. 설정 마법사를 따릅니다:
   - 사용자 이름과 비밀번호를 설정합니다 (SSH용)
   - WiFi를 설정합니다 (SSID, 비밀번호)
   - firmware 업데이트를 적용합니다
5. 표시된 IP 주소를 메모합니다 (예: `arduino@192.168.1.42`) 또는 나중에 App Lab 터미널에서 `ip addr show`로 확인합니다.

### 1.2 SSH 접근 확인

```bash
ssh arduino@<UNO_Q_IP>
# 설정한 비밀번호를 입력합니다
```

---

## 2단계: Uno Q에 ZeroClaw 설치

### 옵션 A: 기기에서 직접 빌드 (더 간단, 약 20~40분)

```bash
# Uno Q에 SSH 접속
ssh arduino@<UNO_Q_IP>

# Rust 설치
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# 빌드 의존성 설치 (Debian)
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev

# zeroclaw 클론 (또는 프로젝트를 scp로 전송)
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# 빌드 (Uno Q에서 약 15~30분 소요)
cargo build --release --features hardware

# 설치
sudo cp target/release/zeroclaw /usr/local/bin/
```

### 옵션 B: Mac에서 크로스 컴파일 (더 빠름)

```bash
# Mac에서 — aarch64 타겟 추가
rustup target add aarch64-unknown-linux-gnu

# 크로스 컴파일러 설치 (macOS; 링킹에 필요)
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu

# 빌드
CC_aarch64_unknown_linux_gnu=aarch64-unknown-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu --features hardware

# Uno Q로 복사
scp target/aarch64-unknown-linux-gnu/release/zeroclaw arduino@<UNO_Q_IP>:~/
ssh arduino@<UNO_Q_IP> "sudo mv ~/zeroclaw /usr/local/bin/"
```

크로스 컴파일이 실패하면 옵션 A를 사용하여 기기에서 직접 빌드합니다.

---

## 3단계: ZeroClaw 설정

### 3.1 온보딩 실행 (또는 수동으로 설정 파일 생성)

```bash
ssh arduino@<UNO_Q_IP>

# 빠른 설정
zeroclaw onboard --api-key YOUR_OPENROUTER_KEY --provider openrouter

# 또는 수동으로 설정 파일 생성
mkdir -p ~/.zeroclaw/workspace
nano ~/.zeroclaw/config.toml
```

### 3.2 최소 config.toml

```toml
api_key = "YOUR_OPENROUTER_API_KEY"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"

[peripherals]
enabled = false
# Bridge를 통한 GPIO는 4단계가 필요합니다

[channels_config.telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
allowed_users = ["*"]

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false

[agent]
compact_context = true
```

---

## 4단계: ZeroClaw 데몬 실행

```bash
ssh arduino@<UNO_Q_IP>

# 데몬 실행 (Telegram 폴링이 WiFi를 통해 작동)
zeroclaw daemon --host 127.0.0.1 --port 42617
```

**이 시점에서:** Telegram 채팅이 작동합니다. 봇에게 메시지를 보내면 ZeroClaw가 응답합니다. 아직 GPIO는 사용할 수 없습니다.

---

## 5단계: Bridge를 통한 GPIO (ZeroClaw가 처리)

ZeroClaw에는 Bridge 앱과 설정 명령어가 포함되어 있습니다.

### 5.1 Bridge 앱 배포

**Mac에서** (zeroclaw 저장소 사용):
```bash
zeroclaw peripheral setup-uno-q --host 192.168.0.48
```

**Uno Q에서** (SSH 접속 상태):
```bash
zeroclaw peripheral setup-uno-q
```

이 명령은 Bridge 앱을 `~/ArduinoApps/uno-q-bridge`에 복사하고 시작합니다.

### 5.2 config.toml에 추가

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "arduino-uno-q"
transport = "bridge"
```

### 5.3 ZeroClaw 실행

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

이제 Telegram 봇에게 *"LED 켜줘"* 또는 *"핀 13을 high로 설정해줘"*라고 메시지를 보내면 ZeroClaw가 Bridge를 통해 `gpio_write`를 사용합니다.

---

## 요약: 처음부터 끝까지 명령어

| 단계 | 명령어 |
|------|---------|
| 1 | App Lab에서 Uno Q 설정 (WiFi, SSH) |
| 2 | `ssh arduino@<IP>` |
| 3 | `curl -sSf https://sh.rustup.rs \| sh -s -- -y && source ~/.cargo/env` |
| 4 | `sudo apt-get install -y pkg-config libssl-dev` |
| 5 | `git clone https://github.com/zeroclaw-labs/zeroclaw.git && cd zeroclaw` |
| 6 | `cargo build --release --features hardware` |
| 7 | `zeroclaw onboard --api-key KEY --provider openrouter` |
| 8 | `~/.zeroclaw/config.toml` 편집 (Telegram bot_token 추가) |
| 9 | `zeroclaw daemon --host 127.0.0.1 --port 42617` |
| 10 | Telegram 봇에 메시지 전송 — 응답 확인 |

---

## 문제 해결

- **"command not found: zeroclaw"** — 전체 경로를 사용합니다: `/usr/local/bin/zeroclaw` 또는 `~/.cargo/bin`이 PATH에 있는지 확인합니다.
- **Telegram이 응답하지 않음** — bot_token, allowed_users를 확인하고 Uno Q가 인터넷에 연결되어 있는지(WiFi) 확인합니다.
- **메모리 부족** — 기능을 최소한으로 유지합니다 (Uno Q에는 `--features hardware`); `compact_context = true` 사용을 고려합니다.
- **GPIO 명령이 무시됨** — Bridge 앱이 실행 중인지 확인합니다 (`zeroclaw peripheral setup-uno-q`가 배포 및 시작). 설정에 `board = "arduino-uno-q"`와 `transport = "bridge"`가 있어야 합니다.
- **LLM 제공자 (GLM/Zhipu)** — `default_provider = "glm"` 또는 `"zhipu"`를 사용하고 환경 변수 또는 설정에 `GLM_API_KEY`를 지정합니다. ZeroClaw가 올바른 v4 엔드포인트를 사용합니다.
