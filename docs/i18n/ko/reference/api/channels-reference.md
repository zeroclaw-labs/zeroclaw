# Channel 레퍼런스

이 문서는 ZeroClaw의 channel 구성에 대한 정식 레퍼런스입니다.

암호화된 Matrix 방에 대해서는 전용 런북도 참조하십시오:
- [Matrix E2EE 가이드](../../security/matrix-e2ee-guide.md)

## 바로 가기

- 채널별 전체 config 레퍼런스가 필요한 경우: [채널별 Config 예시](#4-채널별-config-예시)로 이동하십시오.
- 응답 없음 진단 흐름이 필요한 경우: [문제 해결 체크리스트](#6-문제-해결-체크리스트)로 이동하십시오.
- Matrix 암호화 방 도움이 필요한 경우: [Matrix E2EE 가이드](../../security/matrix-e2ee-guide.md)를 참조하십시오.
- Nextcloud Talk 봇 설정이 필요한 경우: [Nextcloud Talk 설정](../../setup-guides/nextcloud-talk-setup.md)을 참조하십시오.
- 배포/네트워크 전제 조건(polling vs webhook)이 필요한 경우: [네트워크 배포](../../ops/network-deployment.md)를 참조하십시오.

## FAQ: Matrix 설정은 통과했는데 응답이 없는 경우

가장 흔한 증상입니다(이슈 #499와 같은 유형). 다음을 순서대로 확인하십시오:

1. **Allowlist 불일치**: `allowed_users`에 발신자가 포함되지 않았거나 비어 있습니다.
2. **잘못된 방 대상**: 봇이 구성된 `room_id` / 별칭 대상 방에 참여하지 않았습니다.
3. **토큰/계정 불일치**: 토큰은 유효하지만 다른 Matrix 계정에 속합니다.
4. **E2EE 디바이스 ID 누락**: `whoami`가 `device_id`를 반환하지 않고 config에도 지정되지 않았습니다.
5. **키 공유/신뢰 누락**: 방 키가 봇 디바이스에 공유되지 않아 암호화된 이벤트를 복호화할 수 없습니다.
6. **런타임 상태 불일치**: config가 변경되었지만 `zeroclaw daemon`이 재시작되지 않았습니다.

---

## 1. 구성 네임스페이스

모든 channel 설정은 `~/.zeroclaw/config.toml`의 `channels_config` 아래에 위치합니다.

```toml
[channels_config]
cli = true
```

각 channel은 하위 테이블을 생성하여 활성화합니다(예: `[channels_config.telegram]`).

## 채팅 내 런타임 모델 전환 (Telegram / Discord)

`zeroclaw channel start`(또는 daemon 모드) 실행 시, Telegram과 Discord는 발신자 범위의 런타임 전환을 지원합니다:

- `/models` — 사용 가능한 provider와 현재 선택 상태를 표시합니다
- `/models <provider>` — 현재 발신자 세션의 provider를 전환합니다
- `/model` — 현재 모델과 캐시된 모델 ID(가능한 경우)를 표시합니다
- `/model <model-id>` — 현재 발신자 세션의 모델을 전환합니다
- `/new` — 대화 이력을 지우고 새 세션을 시작합니다

참고:

- provider 또는 모델을 전환하면 교차 모델 컨텍스트 오염을 방지하기 위해 해당 발신자의 메모리 내 대화 이력만 삭제됩니다.
- `/new`는 provider나 모델 선택을 변경하지 않고 발신자의 대화 이력만 지웁니다.
- 모델 캐시 미리보기는 `zeroclaw models refresh --provider <ID>`에서 가져옵니다.
- 이들은 런타임 채팅 명령어이며 CLI 하위 명령어가 아닙니다.

## 인바운드 이미지 마커 프로토콜

ZeroClaw는 인라인 메시지 마커를 통한 멀티모달 입력을 지원합니다:

- 구문: ``[IMAGE:<source>]``
- `<source>`로 사용할 수 있는 것:
  - 로컬 파일 경로
  - Data URI (`data:image/...;base64,...`)
  - 원격 URL (`[multimodal].allow_remote_fetch = true`인 경우에만)

운영 참고사항:

- 마커 파싱은 provider 호출 전 user 역할 메시지에 적용됩니다.
- provider 기능은 런타임에 검증됩니다: 선택된 provider가 비전을 지원하지 않으면 구조화된 기능 오류(`capability=vision`)와 함께 요청이 실패합니다.
- Linq webhook `media` 파트의 `image/*` MIME 타입은 자동으로 이 마커 형식으로 변환됩니다.

## Channel Matrix

### 빌드 기능 토글 (`channel-matrix`, `channel-lark`)

Matrix와 Lark 지원은 컴파일 시점에 제어됩니다.

- 기본 빌드는 경량(`default = []`)이며 Matrix/Lark를 포함하지 않습니다.
- 하드웨어 지원만 포함한 일반적인 로컬 검사:

```bash
cargo check --features hardware
```

- 필요할 때 Matrix를 명시적으로 활성화:

```bash
cargo check --features hardware,channel-matrix
```

- 필요할 때 Lark를 명시적으로 활성화:

```bash
cargo check --features hardware,channel-lark
```

`[channels_config.matrix]`, `[channels_config.lark]`, 또는 `[channels_config.feishu]`가 있지만 해당 기능이 컴파일에 포함되지 않은 경우, `zeroclaw channel list`, `zeroclaw channel doctor`, `zeroclaw channel start`는 해당 channel이 이 빌드에서 의도적으로 건너뛰어졌음을 보고합니다.

---

## 2. 전송 모드 요약

| Channel | 수신 모드 | 공용 인바운드 포트 필요 여부 |
|---|---|---|
| CLI | 로컬 stdin/stdout | 아니오 |
| Telegram | polling | 아니오 |
| Discord | gateway/websocket | 아니오 |
| Slack | events API | 아니오 (토큰 기반 channel 흐름) |
| Mattermost | polling | 아니오 |
| Matrix | sync API (E2EE 지원) | 아니오 |
| Signal | signal-cli HTTP 브릿지 | 아니오 (로컬 브릿지 endpoint) |
| WhatsApp | webhook (Cloud API) 또는 websocket (Web 모드) | Cloud API: 예 (공용 HTTPS 콜백), Web 모드: 아니오 |
| Nextcloud Talk | webhook (`/nextcloud-talk`) | 예 (공용 HTTPS 콜백) |
| Webhook | gateway endpoint (`/webhook`) | 보통 예 |
| Email | IMAP polling + SMTP 전송 | 아니오 |
| IRC | IRC 소켓 | 아니오 |
| Lark | websocket (기본) 또는 webhook | webhook 모드에만 필요 |
| Feishu | websocket (기본) 또는 webhook | webhook 모드에만 필요 |
| DingTalk | stream 모드 | 아니오 |
| QQ | 봇 gateway | 아니오 |
| Linq | webhook (`/linq`) | 예 (공용 HTTPS 콜백) |
| iMessage | 로컬 통합 | 아니오 |
| Nostr | relay websocket (NIP-04 / NIP-17) | 아니오 |

---

## 3. Allowlist 동작 규칙

인바운드 발신자 allowlist가 있는 channel의 경우:

- 빈 allowlist: 모든 인바운드 메시지를 거부합니다.
- `"*"`: 모든 인바운드 발신자를 허용합니다(임시 검증용으로만 사용하십시오).
- 명시적 목록: 나열된 발신자만 허용합니다.

필드 이름은 channel마다 다릅니다:

- `allowed_users` (Telegram/Discord/Slack/Mattermost/Matrix/IRC/Lark/Feishu/DingTalk/QQ/Nextcloud Talk)
- `allowed_from` (Signal)
- `allowed_numbers` (WhatsApp)
- `allowed_senders` (Email/Linq)
- `allowed_contacts` (iMessage)
- `allowed_pubkeys` (Nostr)

---

## 4. 채널별 Config 예시

### 4.1 Telegram

```toml
[channels_config.telegram]
bot_token = "123456:telegram-token"
allowed_users = ["*"]
stream_mode = "off"               # 선택 사항: off | partial
draft_update_interval_ms = 1000   # 선택 사항: partial 스트리밍 시 편집 스로틀
mention_only = false              # 선택 사항: 그룹에서 @멘션 필요 여부
interrupt_on_new_message = false  # 선택 사항: 동일 발신자/채팅에서 진행 중인 요청 취소
```

Telegram 참고:

- `interrupt_on_new_message = true`는 중단된 사용자 턴을 대화 이력에 보존한 뒤, 가장 최신 메시지로 생성을 재시작합니다.
- 중단 범위는 엄격합니다: 같은 채팅에서 같은 발신자만 해당됩니다. 다른 채팅의 메시지는 독립적으로 처리됩니다.

### 4.2 Discord

```toml
[channels_config.discord]
bot_token = "discord-bot-token"
guild_id = "123456789012345678"   # 선택 사항
allowed_users = ["*"]
listen_to_bots = false
mention_only = false
stream_mode = "multi_message"     # 선택 사항: off | partial | multi_message (기본: 위저드를 통한 multi_message)
draft_update_interval_ms = 1000   # 선택 사항: partial 스트리밍 시 편집 스로틀
multi_message_delay_ms = 800      # 선택 사항: multi_message 모드에서 문단 전송 간 지연
```

Discord 참고:

- `stream_mode = "partial"`은 LLM이 응답을 스트리밍하면서 토큰 단위로 업데이트되는 편집 가능한 임시 메시지를 전송한 후, 완전한 텍스트로 마무리합니다.
- `stream_mode = "multi_message"`는 응답을 provider로부터 토큰이 도착할 때 문단 경계(`\n\n`)에서 분할하여 별도의 메시지로 점진적으로 전달합니다. 각 문단은 완성되는 즉시 Discord에 표시됩니다.
- `draft_update_interval_ms`는 partial 모드에서의 편집 스로틀을 제어합니다(기본: 1000ms).
- `multi_message_delay_ms`는 Discord 속도 제한을 피하기 위한 multi_message 모드에서의 문단 전송 최소 지연을 제어합니다(기본: 800ms).
- multi_message 모드에서 코드 펜스는 메시지 간에 분할되지 않습니다.

### 4.3 Slack

```toml
[channels_config.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."             # 선택 사항
channel_id = "C1234567890"         # 선택 사항: 단일 channel; 모든 접근 가능한 channel의 경우 생략 또는 "*"
channel_ids = ["C1234567890"]      # 선택 사항: 명시적 channel 목록; channel_id보다 우선
allowed_users = ["*"]
```

Slack 수신 동작:

- `channel_ids = ["C123...", "D456..."]`: 나열된 channel/DM에서만 수신합니다.
- `channel_id = "C123..."`: 해당 channel에서만 수신합니다.
- `channel_id = "*"` 또는 생략: 접근 가능한 모든 channel을 자동 검색하여 수신합니다.

### 4.4 Mattermost

```toml
[channels_config.mattermost]
url = "https://mm.example.com"
bot_token = "mattermost-token"
channel_id = "channel-id"          # 수신에 필수
allowed_users = ["*"]
```

### 4.5 Matrix

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_..."
user_id = "@zeroclaw:matrix.example.com"   # 선택 사항, E2EE에 권장
device_id = "DEVICEID123"                  # 선택 사항, E2EE에 권장
room_id = "!room:matrix.example.com"       # 또는 방 별칭 (#ops:matrix.example.com)
allowed_users = ["*"]
stream_mode = "partial"                    # 선택 사항: off | partial | multi_message (기본: 위저드를 통한 partial)
draft_update_interval_ms = 1500            # 선택 사항: partial 스트리밍 시 편집 스로틀
multi_message_delay_ms = 800               # 선택 사항: multi_message 모드에서 문단 전송 간 지연
```

Matrix 스트리밍 참고:

- `stream_mode = "partial"`은 LLM이 응답을 스트리밍하면서 Matrix `m.replace` 편집을 통해 토큰 단위로 업데이트되는 편집 가능한 임시 메시지를 전송합니다.
- `stream_mode = "multi_message"`는 응답을 토큰이 도착할 때 문단 경계(`\n\n`)에서 분할하여 별도의 메시지로 점진적으로 전달합니다. 코드 펜스는 메시지 간에 분할되지 않습니다.
- `draft_update_interval_ms`는 partial 모드에서의 편집 스로틀을 제어합니다(기본: 1500ms, E2EE 재암호화 오버헤드와 페더레이션 지연을 고려하여 Telegram보다 높음).
- `multi_message_delay_ms`는 multi_message 모드에서의 문단 전송 최소 지연을 제어합니다(기본: 800ms).
- 두 모드 모두 암호화 및 비암호화 방에서 작동합니다 -- matrix-sdk가 E2EE를 투명하게 처리합니다.
- `stream_mode` 없는 기존 config는 기본값 `off`로 동작합니다(동작 변경 없음).

암호화된 방 문제 해결은 [Matrix E2EE 가이드](../../security/matrix-e2ee-guide.md)를 참조하십시오.

### 4.6 Signal

```toml
[channels_config.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_id = "dm"                    # 선택 사항: "dm" / group id / 생략
allowed_from = ["*"]
ignore_attachments = false
ignore_stories = true
```

### 4.7 WhatsApp

ZeroClaw는 두 가지 WhatsApp 백엔드를 지원합니다:

- **Cloud API 모드** (`phone_number_id` + `access_token` + `verify_token`)
- **WhatsApp Web 모드** (`session_path`, 빌드 플래그 `--features whatsapp-web` 필요)

Cloud API 모드:

```toml
[channels_config.whatsapp]
access_token = "EAAB..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
app_secret = "your-app-secret"     # 선택 사항이나 권장
allowed_numbers = ["*"]
```

WhatsApp Web 모드:

```toml
[channels_config.whatsapp]
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
pair_phone = "15551234567"         # 선택 사항; QR 플로우를 사용하려면 생략
pair_code = ""                     # 선택 사항 커스텀 페어 코드
allowed_numbers = ["*"]
mention_only = false               # 선택 사항: 그룹에서 @멘션 필요 여부 (DM은 항상 처리)
interrupt_on_new_message = false   # 선택 사항: 동일 발신자/채팅에서 진행 중인 요청 취소
```

참고:

- `cargo build --features whatsapp-web`(또는 동등한 실행 명령)으로 빌드하십시오.
- 재시작 후 재연결을 방지하려면 `session_path`를 영구 스토리지에 유지하십시오.
- 응답 라우팅은 원본 채팅 JID를 사용하므로 1:1 및 그룹 응답이 올바르게 작동합니다.
- `mention_only = true`는 봇이 @멘션되지 않은 그룹 메시지를 무시하게 합니다. 1:1 메시지는 항상 처리됩니다. 봇 ID는 `pair_phone`에서 초기화되고 연결 시 디바이스 저장소에서 업데이트됩니다.
- `interrupt_on_new_message = true`는 중단된 사용자 턴을 대화 이력에 보존한 뒤, 가장 최신 메시지로 생성을 재시작합니다.

### 4.8 Webhook Channel Config (Gateway)

`channels_config.webhook`은 webhook 전용 gateway 동작을 활성화합니다.

```toml
[channels_config.webhook]
port = 8080
secret = "optional-shared-secret"
```

gateway/daemon으로 실행하고 `/health`를 확인하십시오.

### 4.9 Email

```toml
[channels_config.email]
imap_host = "imap.example.com"
imap_port = 993
imap_folder = "INBOX"
smtp_host = "smtp.example.com"
smtp_port = 465
smtp_tls = true
username = "bot@example.com"
password = "email-password"
from_address = "bot@example.com"
poll_interval_secs = 60
allowed_senders = ["*"]
```

### 4.10 IRC

```toml
[channels_config.irc]
server = "irc.libera.chat"
port = 6697
nickname = "zeroclaw-bot"
username = "zeroclaw"              # 선택 사항
channels = ["#zeroclaw"]
allowed_users = ["*"]
server_password = ""                # 선택 사항
nickserv_password = ""              # 선택 사항
sasl_password = ""                  # 선택 사항
verify_tls = true
```

### 4.11 Lark

```toml
[channels_config.lark]
app_id = "cli_xxx"
app_secret = "xxx"
encrypt_key = ""                    # 선택 사항
verification_token = ""             # 선택 사항
allowed_users = ["*"]
mention_only = false              # 선택 사항: 그룹에서 @멘션 필요 여부 (DM은 항상 허용)
use_feishu = false
receive_mode = "websocket"          # 또는 "webhook"
port = 8081                          # webhook 모드에 필수
```

### 4.12 Feishu

```toml
[channels_config.feishu]
app_id = "cli_xxx"
app_secret = "xxx"
encrypt_key = ""                    # 선택 사항
verification_token = ""             # 선택 사항
allowed_users = ["*"]
receive_mode = "websocket"          # 또는 "webhook"
port = 8081                          # webhook 모드에 필수
```

마이그레이션 참고:

- 레거시 config `[channels_config.lark] use_feishu = true`는 하위 호환성을 위해 계속 지원됩니다.
- 새로운 설정에는 `[channels_config.feishu]`를 선호하십시오.

### 4.13 Nostr

```toml
[channels_config.nostr]
private_key = "nsec1..."                   # hex 또는 nsec bech32 (저장 시 암호화)
# relays 기본값: relay.damus.io, nos.lol, relay.primal.net, relay.snort.social
# relays = ["wss://relay.damus.io", "wss://nos.lol"]
allowed_pubkeys = ["hex-or-npub"]          # 비어 있으면 모두 거부, "*"이면 모두 허용
```

Nostr는 NIP-04 (레거시 암호화 DM)와 NIP-17 (선물 포장 비공개 메시지)를 모두 지원합니다.
응답은 발신자가 사용한 프로토콜을 자동으로 따릅니다. 비공개 키는 `secrets.encrypt = true`(기본값)일 때 `SecretStore`를 통해 저장 시 암호화됩니다.

가이드 온보딩 지원:

```bash
zeroclaw onboard
```

위저드에는 이제 전용 **Lark** 및 **Feishu** 단계가 포함되어 있으며 다음을 제공합니다:

- 공식 Open Platform 인증 endpoint에 대한 자격 증명 검증
- 수신 모드 선택(`websocket` 또는 `webhook`)
- 선택적 webhook 검증 토큰 프롬프트(더 강력한 콜백 인증 검사에 권장)

런타임 토큰 동작:

- `tenant_access_token`은 인증 응답의 `expire`/`expires_in`을 기반으로 갱신 기한과 함께 캐시됩니다.
- 전송 요청은 Feishu/Lark가 HTTP `401` 또는 비즈니스 오류 코드 `99991663`(`Invalid access token`)을 반환할 때 토큰 무효화 후 자동으로 한 번 재시도합니다.
- 재시도 후에도 토큰 무효 응답이 반환되면, 더 쉬운 문제 해결을 위해 업스트림 상태/본문과 함께 전송 호출이 실패합니다.

### 4.14 DingTalk

```toml
[channels_config.dingtalk]
client_id = "ding-app-key"
client_secret = "ding-app-secret"
allowed_users = ["*"]
```

### 4.15 QQ

```toml
[channels_config.qq]
app_id = "qq-app-id"
app_secret = "qq-app-secret"
allowed_users = ["*"]
```

### 4.16 Nextcloud Talk

```toml
[channels_config.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "nextcloud-talk-app-token"
webhook_secret = "optional-webhook-secret"  # 선택 사항이나 권장
allowed_users = ["*"]
# bot_name = "zeroclaw"  # 봇 표시 이름; 피드백 루프를 방지하기 위해 자체 메시지를 필터링합니다
```

참고:

- 인바운드 webhook endpoint: `POST /nextcloud-talk`.
- 서명 검증은 `X-Nextcloud-Talk-Random`과 `X-Nextcloud-Talk-Signature`를 사용합니다.
- `webhook_secret`이 설정된 경우 유효하지 않은 서명은 `401`로 거부됩니다.
- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET`는 config 시크릿을 오버라이드합니다.
- 전체 런북은 [nextcloud-talk-setup.md](../../setup-guides/nextcloud-talk-setup.md)를 참조하십시오.

### 4.16 Linq

```toml
[channels_config.linq]
api_token = "linq-partner-api-token"
from_phone = "+15551234567"
signing_secret = "optional-webhook-signing-secret"  # 선택 사항이나 권장
allowed_senders = ["*"]
```

참고:

- Linq는 iMessage, RCS, SMS를 위한 Partner V3 API를 사용합니다.
- 인바운드 webhook endpoint: `POST /linq`.
- 서명 검증은 `X-Webhook-Signature` (HMAC-SHA256)와 `X-Webhook-Timestamp`를 사용합니다.
- `signing_secret`이 설정된 경우 유효하지 않거나 오래된(>300초) 서명은 거부됩니다.
- `ZEROCLAW_LINQ_SIGNING_SECRET`는 config 시크릿을 오버라이드합니다.
- `allowed_senders`는 E.164 전화번호 형식을 사용합니다(예: `+1234567890`).

### 4.17 iMessage

```toml
[channels_config.imessage]
allowed_contacts = ["*"]
```

---

## 5. 검증 워크플로

1. 초기 검증을 위해 허용적인 allowlist(`"*"`)로 하나의 channel을 구성합니다.
2. 실행:

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

1. 예상 발신자로부터 메시지를 보냅니다.
2. 응답이 도착하는지 확인합니다.
3. allowlist를 `"*"`에서 명시적 ID로 좁힙니다.

---

## 6. 문제 해결 체크리스트

channel이 연결된 것으로 보이지만 응답하지 않는 경우:

1. 올바른 allowlist 필드에 발신자 ID가 허용되어 있는지 확인합니다.
2. 대상 방/channel에서 봇 계정의 멤버십/권한을 확인합니다.
3. 토큰/시크릿이 유효한지(만료/폐기되지 않았는지) 확인합니다.
4. 전송 모드 전제 조건을 확인합니다:
   - polling/websocket channel은 공용 인바운드 HTTP가 필요 없습니다
   - webhook channel은 도달 가능한 HTTPS 콜백이 필요합니다
5. config 변경 후 `zeroclaw daemon`을 재시작합니다.

Matrix 암호화 방에 대해서는 다음을 참조하십시오:
- [Matrix E2EE 가이드](../../security/matrix-e2ee-guide.md)

---

## 7. 운영 부록: 로그 키워드 매트릭스

빠른 분류를 위해 이 부록을 사용하십시오. 먼저 로그 키워드를 매칭한 후, 위의 문제 해결 단계를 따르십시오.

### 7.1 권장 캡처 명령

```bash
RUST_LOG=info zeroclaw daemon 2>&1 | tee /tmp/zeroclaw.log
```

그런 다음 channel/gateway 이벤트를 필터링합니다:

```bash
rg -n "Matrix|Telegram|Discord|Slack|Mattermost|Signal|WhatsApp|Email|IRC|Lark|DingTalk|QQ|iMessage|Nostr|Webhook|Channel" /tmp/zeroclaw.log
```

### 7.2 키워드 표

| 컴포넌트 | 시작 / 정상 신호 | 인증 / 정책 신호 | 전송 / 장애 신호 |
|---|---|---|---|
| Telegram | `Telegram channel listening for messages...` | `Telegram: ignoring message from unauthorized user:` | `Telegram poll error:` / `Telegram parse error:` / `Telegram polling conflict (409):` |
| Discord | `Discord: connected and identified` | `Discord: ignoring message from unauthorized user:` | `Discord: received Reconnect (op 7)` / `Discord: received Invalid Session (op 9)` |
| Slack | `Slack channel listening on #` / `Slack channel_id not set (or '*'); listening across all accessible channels.` | `Slack: ignoring message from unauthorized user:` | `Slack poll error:` / `Slack parse error:` / `Slack channel discovery failed:` |
| Mattermost | `Mattermost channel listening on` | `Mattermost: ignoring message from unauthorized user:` | `Mattermost poll error:` / `Mattermost parse error:` |
| Matrix | `Matrix channel listening on room` / `Matrix room ... is encrypted; E2EE decryption is enabled via matrix-sdk.` | `Matrix whoami failed; falling back to configured session hints for E2EE session restore:` / `Matrix whoami failed while resolving listener user_id; using configured user_id hint:` | `Matrix sync error: ... retrying...` |
| Signal | `Signal channel listening via SSE on` | (allowlist 검사는 `allowed_from`으로 적용됨) | `Signal SSE returned ...` / `Signal SSE connect error:` |
| WhatsApp (channel) | `WhatsApp channel active (webhook mode).` / `WhatsApp Web connected successfully` | `WhatsApp: ignoring message from unauthorized number:` / `WhatsApp Web: message from ... not in allowed list` | `WhatsApp send failed:` / `WhatsApp Web stream error:` |
| Webhook / WhatsApp (gateway) | `WhatsApp webhook verified successfully` | `Webhook: rejected — not paired / invalid bearer token` / `Webhook: rejected request — invalid or missing X-Webhook-Secret` / `WhatsApp webhook verification failed — token mismatch` | `Webhook JSON parse error:` |
| Email | `Email polling every ...` / `Email sent to ...` | `Blocked email from ...` | `Email poll failed:` / `Email poll task panicked:` |
| IRC | `IRC channel connecting to ...` / `IRC registered as ...` | (allowlist 검사는 `allowed_users`로 적용됨) | `IRC SASL authentication failed (...)` / `IRC server does not support SASL...` / `IRC nickname ... is in use, trying ...` |
| Lark / Feishu | `Lark: WS connected` / `Lark event callback server listening on` | `Lark WS: ignoring ... (not in allowed_users)` / `Lark: ignoring message from unauthorized user:` | `Lark: ping failed, reconnecting` / `Lark: heartbeat timeout, reconnecting` / `Lark: WS read error:` |
| DingTalk | `DingTalk: connected and listening for messages...` | `DingTalk: ignoring message from unauthorized user:` | `DingTalk WebSocket error:` / `DingTalk: message channel closed` |
| QQ | `QQ: connected and identified` | `QQ: ignoring C2C message from unauthorized user:` / `QQ: ignoring group message from unauthorized user:` | `QQ: received Reconnect (op 7)` / `QQ: received Invalid Session (op 9)` / `QQ: message channel closed` |
| Nextcloud Talk (gateway) | `POST /nextcloud-talk — Nextcloud Talk bot webhook` | `Nextcloud Talk webhook signature verification failed` / `Nextcloud Talk: ignoring message from unauthorized actor:` | `Nextcloud Talk send failed:` / `LLM error for Nextcloud Talk message:` |
| iMessage | `iMessage channel listening (AppleScript bridge)...` | (연락처 allowlist는 `allowed_contacts`로 적용됨) | `iMessage poll error:` |
| Nostr | `Nostr channel listening as npub1...` | `Nostr: ignoring NIP-04 message from unauthorized pubkey:` / `Nostr: ignoring NIP-17 message from unauthorized pubkey:` | `Failed to decrypt NIP-04 message:` / `Failed to unwrap NIP-17 gift wrap:` / `Nostr relay pool shut down` |

### 7.3 런타임 감독자 키워드

특정 channel 태스크가 충돌하거나 종료되면, `channels/mod.rs`의 channel 감독자가 다음을 출력합니다:

- `Channel <name> exited unexpectedly; restarting`
- `Channel <name> error: ...; restarting`
- `Channel message worker crashed:`

이 메시지들은 자동 재시작 동작이 활성화되어 있음을 나타내며, 근본 원인을 파악하려면 앞선 로그를 확인해야 합니다.
