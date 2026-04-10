# 네트워크 Deployment — Raspberry Pi 및 로컬 네트워크에서의 ZeroClaw

이 문서는 Raspberry Pi 또는 로컬 네트워크의 다른 호스트에 ZeroClaw를 배포하는 방법을 다루며, Telegram 및 선택적 webhook 채널을 포함합니다.

---

## 1. 개요

| 모드 | 인바운드 포트 필요 여부 | 사용 사례 |
|------|----------------------|----------|
| **Telegram polling** | 아니오 | ZeroClaw가 Telegram API를 폴링하므로 어디서나 동작 |
| **Matrix sync (E2EE 포함)** | 아니오 | ZeroClaw가 Matrix 클라이언트 API를 통해 동기화하므로 인바운드 webhook 불필요 |
| **Discord/Slack** | 아니오 | 동일 — 아웃바운드 전용 |
| **Nostr** | 아니오 | WebSocket을 통해 릴레이에 연결하므로 아웃바운드 전용 |
| **Gateway webhook** | 예 | POST /webhook, /whatsapp, /linq, /nextcloud-talk에는 공개 URL 필요 |
| **Gateway 페어링** | 예 | gateway를 통해 클라이언트를 페어링하는 경우 |
| **Alpine/OpenRC 서비스** | 아니오 | Alpine Linux에서 시스템 전체 백그라운드 서비스 |

**핵심:** Telegram, Discord, Slack, Nostr는 **아웃바운드 연결**을 사용합니다 — ZeroClaw가 외부 서버/릴레이에 연결합니다. 포트 포워딩이나 공인 IP가 필요하지 않습니다.

---

## 2. Raspberry Pi에서의 ZeroClaw

### 2.1 사전 요구 사항

- Raspberry Pi (3/4/5), Raspberry Pi OS 설치
- USB 주변기기 (Arduino, Nucleo) — 시리얼 전송을 사용하는 경우
- 선택 사항: 네이티브 GPIO를 위한 `rppal` (`peripheral-rpi` feature)

### 2.2 설치

```bash
# RPi용 빌드 (또는 호스트에서 크로스 컴파일)
cargo build --release --features hardware

# 또는 선호하는 방법으로 설치
```

### 2.3 설정

`~/.zeroclaw/config.toml` 편집:

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"

# 또는 USB를 통한 Arduino
[[peripherals.boards]]
board = "arduino-uno"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = []

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false
```

### 2.4 데몬 실행 (로컬 전용)

```bash
zeroclaw daemon --host 127.0.0.1 --port 42617
```

- Gateway가 `127.0.0.1`에 바인딩 — 다른 머신에서 접근 불가
- Telegram 채널은 정상 동작: ZeroClaw가 Telegram API를 폴링 (아웃바운드)
- 방화벽이나 포트 포워딩이 필요하지 않음

---

## 3. 0.0.0.0에 바인딩 (로컬 네트워크)

LAN의 다른 기기에서 gateway에 접근하려면 (예: 페어링 또는 webhook):

### 3.1 옵션 A: 명시적 동의

```toml
[gateway]
host = "0.0.0.0"
port = 42617
allow_public_bind = true
```

```bash
zeroclaw daemon --host 0.0.0.0 --port 42617
```

**보안:** `allow_public_bind = true`는 gateway를 로컬 네트워크에 노출합니다. 신뢰할 수 있는 LAN에서만 사용하십시오.

### 3.2 옵션 B: 터널 (webhook에 권장)

**공개 URL**이 필요한 경우 (예: WhatsApp webhook, 외부 클라이언트):

1. localhost에서 gateway 실행:
   ```bash
   zeroclaw daemon --host 127.0.0.1 --port 42617
   ```

2. 터널 시작:
   ```toml
   [tunnel]
   provider = "tailscale"   # 또는 "ngrok", "cloudflare"
   ```
   또는 `zeroclaw tunnel`을 사용하십시오 (터널 문서 참조).

3. `allow_public_bind = true`이거나 터널이 활성화되어 있지 않으면 ZeroClaw는 `0.0.0.0`을 거부합니다.

---

## 4. Telegram Polling (인바운드 포트 불필요)

Telegram은 기본적으로 **long-polling**을 사용합니다:

- ZeroClaw가 `https://api.telegram.org/bot{token}/getUpdates`를 호출
- 인바운드 포트나 공인 IP가 필요하지 않음
- NAT 뒤, RPi, 홈 랩에서 모두 동작

**설정:**

```toml
[channels_config.telegram]
bot_token = "YOUR_BOT_TOKEN"
allowed_users = []            # 기본적으로 거부, identity를 명시적으로 바인딩
```

`zeroclaw daemon`을 실행하면 Telegram 채널이 자동으로 시작됩니다.

런타임에서 Telegram 계정 하나를 승인하려면:

```bash
zeroclaw channel bind-telegram <IDENTITY>
```

`<IDENTITY>`는 숫자형 Telegram 사용자 ID 또는 사용자 이름(`@` 제외)이 될 수 있습니다.

### 4.1 단일 폴러 규칙 (중요)

Telegram Bot API의 `getUpdates`는 봇 토큰당 하나의 활성 폴러만 지원합니다.

- 동일 토큰에 대해 하나의 런타임 인스턴스만 유지하십시오 (권장: `zeroclaw daemon` 서비스).
- `cargo run -- channel start` 또는 다른 봇 프로세스를 동시에 실행하지 마십시오.

다음 오류가 발생하는 경우:

`Conflict: terminated by other getUpdates request`

폴링 충돌이 있습니다. 추가 인스턴스를 중지하고 하나의 데몬만 재시작하십시오.

---

## 5. Webhook 채널 (WhatsApp, Nextcloud Talk, 커스텀)

Webhook 기반 채널은 Meta (WhatsApp) 또는 클라이언트가 이벤트를 POST할 수 있도록 **공개 URL**이 필요합니다.

### 5.1 Tailscale Funnel

```toml
[tunnel]
provider = "tailscale"
```

Tailscale Funnel은 `*.ts.net` URL을 통해 gateway를 노출합니다. 포트 포워딩이 필요하지 않습니다.

### 5.2 ngrok

```toml
[tunnel]
provider = "ngrok"
```

또는 ngrok를 수동으로 실행:
```bash
ngrok http 42617
# webhook에 HTTPS URL을 사용
```

### 5.3 Cloudflare Tunnel

Cloudflare Tunnel을 `127.0.0.1:42617`로 포워딩하도록 구성한 다음, webhook URL을 터널의 공개 호스트 이름으로 설정하십시오.

---

## 6. 체크리스트: RPi Deployment

- [ ] `--features hardware` (네이티브 GPIO 사용 시 `peripheral-rpi` 포함)로 빌드
- [ ] `[peripherals]` 및 `[channels_config.telegram]` 구성
- [ ] `zeroclaw daemon --host 127.0.0.1 --port 42617` 실행 (Telegram은 0.0.0.0 없이 동작)
- [ ] LAN 접근 시: `--host 0.0.0.0` + 설정에서 `allow_public_bind = true`
- [ ] webhook 사용 시: Tailscale, ngrok 또는 Cloudflare 터널 사용

---

## 7. OpenRC (Alpine Linux 서비스)

ZeroClaw는 Alpine Linux 및 OpenRC init 시스템을 사용하는 기타 배포판에서 OpenRC를 지원합니다. OpenRC 서비스는 **시스템 전체**로 실행되며 root/sudo가 필요합니다.

### 7.1 사전 요구 사항

- Alpine Linux (또는 다른 OpenRC 기반 배포판)
- root 또는 sudo 접근 권한
- 전용 `zeroclaw` 시스템 사용자 (설치 중 생성)

### 7.2 서비스 설치

```bash
# 서비스 설치 (Alpine에서 OpenRC가 자동 감지됨)
sudo zeroclaw service install
```

다음 항목이 생성됩니다:
- init 스크립트: `/etc/init.d/zeroclaw`
- 설정 디렉터리: `/etc/zeroclaw/`
- 로그 디렉터리: `/var/log/zeroclaw/`

### 7.3 설정

수동 설정 복사는 일반적으로 필요하지 않습니다.

`sudo zeroclaw service install`은 자동으로 `/etc/zeroclaw`를 준비하고, 사용 가능한 경우 기존 사용자 설정에서 런타임 상태를 마이그레이션하며, `zeroclaw` 서비스 사용자의 소유권/권한을 설정합니다.

마이그레이션할 기존 런타임 상태가 없는 경우, 서비스 시작 전에 `/etc/zeroclaw/config.toml`을 생성하십시오.

### 7.4 활성화 및 시작

```bash
# 기본 런레벨에 추가
sudo rc-update add zeroclaw default

# 서비스 시작
sudo rc-service zeroclaw start

# 상태 확인
sudo rc-service zeroclaw status
```

### 7.5 서비스 관리

| 명령어 | 설명 |
|---------|-------------|
| `sudo rc-service zeroclaw start` | 데몬 시작 |
| `sudo rc-service zeroclaw stop` | 데몬 중지 |
| `sudo rc-service zeroclaw status` | 서비스 상태 확인 |
| `sudo rc-service zeroclaw restart` | 데몬 재시작 |
| `sudo zeroclaw service status` | ZeroClaw 상태 래퍼 (`/etc/zeroclaw` 설정 사용) |

### 7.6 로그

OpenRC는 로그를 다음 경로로 라우팅합니다:

| 로그 | 경로 |
|-----|------|
| 접근/stdout | `/var/log/zeroclaw/access.log` |
| 오류/stderr | `/var/log/zeroclaw/error.log` |

로그 확인:

```bash
sudo tail -f /var/log/zeroclaw/error.log
```

### 7.7 제거

```bash
# 중지 및 런레벨에서 제거
sudo rc-service zeroclaw stop
sudo rc-update del zeroclaw default

# init 스크립트 제거
sudo zeroclaw service uninstall
```

### 7.8 참고 사항

- OpenRC는 **시스템 전체 전용**입니다 (사용자 수준 서비스 없음)
- 모든 서비스 작업에 `sudo` 또는 root가 필요합니다
- 서비스는 `zeroclaw:zeroclaw` 사용자로 실행됩니다 (최소 권한)
- 설정은 `/etc/zeroclaw/config.toml`에 있어야 합니다 (init 스크립트에 명시된 경로)
- `zeroclaw` 사용자가 없으면 설치가 실패하며 생성 안내가 표시됩니다

### 7.9 체크리스트: Alpine/OpenRC Deployment

- [ ] 설치: `sudo zeroclaw service install`
- [ ] 활성화: `sudo rc-update add zeroclaw default`
- [ ] 시작: `sudo rc-service zeroclaw start`
- [ ] 확인: `sudo rc-service zeroclaw status`
- [ ] 로그 확인: `/var/log/zeroclaw/error.log`

---

## 8. 참조 문서

- [channels-reference.md](../reference/api/channels-reference.md) — 채널 설정 개요
- [matrix-e2ee-guide.md](../security/matrix-e2ee-guide.md) — Matrix 설정 및 암호화 채팅방 문제 해결
- [hardware-peripherals-design.md](../hardware/hardware-peripherals-design.md) — 주변기기 설계
- [adding-boards-and-tools.md](../contributing/adding-boards-and-tools.md) — 하드웨어 설정 및 보드 추가
