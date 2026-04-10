# Matrix E2EE 가이드

이 가이드는 E2EE(종단 간 암호화) 방을 포함하여 Matrix 방에서 ZeroClaw를 안정적으로 운영하는 방법을 설명합니다.

사용자들이 흔히 보고하는 장애 모드에 초점을 맞추고 있습니다:

> "Matrix가 올바르게 설정되어 있고, 검사도 통과하지만, 봇이 응답하지 않습니다."

## 0. 빠른 FAQ (#499 류 증상)

Matrix가 연결된 것으로 보이지만 응답이 없는 경우, 다음을 먼저 확인하십시오:

1. 발신자가 `allowed_users`에 허용되어 있는지 확인 (테스트용: `["*"]`).
2. 봇 계정이 정확한 대상 방에 참여했는지 확인.
3. 토큰이 동일한 봇 계정에 속하는지 확인 (`whoami` 검사).
4. 암호화된 방에서 사용 가능한 디바이스 ID(`device_id`)와 키 공유가 되어 있는지 확인.
5. 설정 변경 후 데몬을 재시작했는지 확인.

---

## 1. 요구 사항

메시지 흐름을 테스트하기 전에 다음 사항이 모두 참인지 확인하십시오:

1. 봇 계정이 대상 방에 참여해 있어야 합니다.
2. 접근 토큰이 동일한 봇 계정에 속해야 합니다.
3. `room_id`가 올바른지 확인:
   - 권장: 정규 방 ID (`!room:server`)
   - 지원: 방 별칭 (`#alias:server`) — ZeroClaw가 자동으로 해석합니다
4. `allowed_users`가 발신자를 허용해야 합니다 (공개 테스트용 `["*"]`).
5. E2EE 방의 경우, 봇 디바이스가 해당 방의 암호화 키를 수신해야 합니다.

---

## 2. 설정

`~/.zeroclaw/config.toml`을 사용하십시오:

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_your_token"

# 선택 사항이지만 E2EE 안정성을 위해 권장:
user_id = "@zeroclaw:matrix.example.com"
device_id = "DEVICEID123"

# 방 ID 또는 별칭
room_id = "!xtHhdHIIVEZbDPvTvZ:matrix.example.com"
# room_id = "#ops:matrix.example.com"

# 초기 검증 시 ["*"]를 사용한 후 점차 제한하십시오.
allowed_users = ["*"]
```

### `user_id`와 `device_id`에 대하여

- ZeroClaw는 Matrix `/_matrix/client/v3/account/whoami`에서 ID를 읽으려고 시도합니다.
- `whoami`가 `device_id`를 반환하지 않으면 `device_id`를 수동으로 설정하십시오.
- 이 힌트들은 특히 E2EE 세션 복원에 중요합니다.

---

## 3. 빠른 검증 절차

1. 채널 설정 및 데몬 실행:

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

2. 설정된 Matrix 방에서 일반 텍스트 메시지를 보냅니다.

3. ZeroClaw 로그에 Matrix 리스너 시작이 포함되어 있고 반복되는 sync/auth 오류가 없는지 확인합니다.

4. 암호화된 방에서 봇이 허용된 사용자의 암호화된 메시지를 읽고 응답할 수 있는지 확인합니다.

---

## 4. "응답 없음" 문제 해결

다음 체크리스트를 순서대로 확인하십시오.

### A. 방과 멤버십

- 봇 계정이 방에 참여했는지 확인합니다.
- 별칭(`#...`)을 사용하는 경우, 예상된 정규 방으로 해석되는지 확인합니다.

### B. 발신자 allowlist

- `allowed_users = []`이면 모든 수신 메시지가 거부됩니다.
- 진단을 위해 임시로 `allowed_users = ["*"]`로 설정하십시오.

### C. 토큰과 ID

- 다음으로 토큰을 검증합니다:

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://matrix.example.com/_matrix/client/v3/account/whoami"
```

- 반환된 `user_id`가 봇 계정과 일치하는지 확인합니다.
- `device_id`가 누락된 경우, `channels_config.matrix.device_id`를 수동으로 설정합니다.
- onboard를 다시 실행하지 않고 접근 토큰을 업데이트하려면:
  ```bash
  zeroclaw props set channels.matrix.access-token
  ```

### D. E2EE 관련 확인 사항

- 봇 디바이스가 신뢰할 수 있는 디바이스로부터 방 키를 수신해야 합니다.
- 이 디바이스에 키가 공유되지 않으면 암호화된 이벤트를 복호화할 수 없습니다.
- Matrix 클라이언트/관리자 워크플로우에서 디바이스 신뢰 및 키 공유를 확인하십시오.
- 로그에 `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found`가 표시되면, 이 디바이스에 아직 키 백업 복구가 활성화되지 않은 것입니다. 이 경고는 일반적으로 실시간 메시지 흐름에 치명적이지 않지만, 키 백업/복구 설정을 완료하는 것이 좋습니다.
- 수신자가 봇 메시지를 "검증되지 않음"으로 볼 경우, 신뢰할 수 있는 Matrix 세션에서 봇 디바이스를 검증/서명하고 재시작 시에도 `channels_config.matrix.device_id`를 안정적으로 유지하십시오.

### E. 메시지 포맷 (Markdown)

- ZeroClaw는 Matrix 텍스트 응답을 markdown 지원 `m.room.message` 텍스트 콘텐츠로 전송합니다.
- `formatted_body`를 지원하는 Matrix 클라이언트는 강조, 목록, 코드 블록을 렌더링합니다.
- 포맷이 일반 텍스트로 나타나면, 먼저 클라이언트 기능을 확인한 후 ZeroClaw가 markdown 지원 Matrix 출력이 포함된 빌드를 실행하고 있는지 확인하십시오.

### F. 새로 시작 테스트

설정 업데이트 후 데몬을 재시작하고 새 메시지를 보내십시오 (이전 타임라인 기록이 아닌).

### G. `device_id` 찾기

ZeroClaw는 E2EE 세션 복원을 위해 안정적인 `device_id`가 필요합니다. 이것이 없으면 매 재시작마다 새 디바이스가 등록되어 키 공유와 디바이스 검증이 중단됩니다.

#### 옵션 1: `whoami`에서 가져오기 (가장 쉬움)

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://your.homeserver/_matrix/client/v3/account/whoami"
```

토큰이 디바이스 세션에 바인딩된 경우 응답에 `device_id`가 포함됩니다:

```json
{"user_id": "@bot:example.com", "device_id": "ABCDEF1234"}
```

`device_id`가 누락된 경우, 토큰이 디바이스 로그인 없이 생성된 것입니다 (예: 관리자 API를 통해). 대신 옵션 2를 사용하십시오.

#### 옵션 2: 비밀번호 로그인에서 가져오기

```bash
curl -sS -X POST "https://your.homeserver/_matrix/client/v3/login" \
  -H "Content-Type: application/json" \
  -d '{"type": "m.login.password", "user": "@bot:example.com", "password": "...", "initial_device_display_name": "ZeroClaw"}'
```

응답:

```json
{"user_id": "@bot:example.com", "access_token": "syt_...", "device_id": "NEWDEVICE"}
```

반환된 `access_token`과 `device_id`를 모두 설정에 사용하십시오. 이렇게 하면 적절한 디바이스 세션이 생성됩니다.

#### 옵션 3: Element 또는 다른 Matrix 클라이언트에서 가져오기

1. Element에 봇 계정으로 로그인합니다
2. 설정 → 세션으로 이동합니다
3. 활성 세션의 디바이스 ID를 복사합니다

**가져온 후**, `config.toml`에 둘 다 설정하십시오:

```toml
[channels_config.matrix]
user_id = "@bot:example.com"
device_id = "ABCDEF1234"
```

`device_id`를 안정적으로 유지하십시오 — 변경하면 새 디바이스 등록이 강제되어 기존 키 공유와 디바이스 검증이 중단됩니다.

### H. 일회용 키 (OTK) 업로드 충돌

**증상:** ZeroClaw 로그에 `Matrix one-time key upload conflict detected; stopping sync to avoid infinite retry loop.`이 표시되고 Matrix 채널을 사용할 수 없게 됩니다.

**원인:** 봇의 로컬 암호화 저장소가 홈서버에서 이전 디바이스를 해제하지 않은 채 초기화되었습니다 (예: 데이터 디렉토리 삭제, 재설치). 홈서버에는 여전히 이 디바이스의 이전 일회용 키가 있으며, SDK가 새 키를 업로드하지 못합니다.

#### 해결 방법

1. ZeroClaw를 중지합니다.

2. 만료된 디바이스를 해제합니다. 봇 계정에 대한 관리자 접근 권한이 있는 세션에서:

```bash
# 디바이스 목록 조회
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://your.homeserver/_matrix/client/v3/devices"

# 만료된 디바이스 삭제 (UIA — 대화형 인증 필요)
curl -sS -X DELETE -H "Authorization: Bearer $MATRIX_TOKEN" \
  -H "Content-Type: application/json" \
  "https://your.homeserver/_matrix/client/v3/devices/STALE_DEVICE_ID" \
  -d '{"auth": {"type": "m.login.password", "user": "@bot:example.com", "password": "..."}}'
```

3. 로컬 암호화 저장소를 삭제합니다. 로그 메시지에 저장소 경로가 포함되어 있으며, 일반적으로:

```
~/.zeroclaw/state/matrix/
```

이 디렉토리를 삭제합니다.

4. 새 `device_id`와 `access_token`을 얻기 위해 재로그인합니다 (섹션 4G, 옵션 2 참조).

5. `config.toml`을 새 `access_token`과 `device_id`로 업데이트합니다.

6. ZeroClaw를 재시작합니다.

**예방:** 디바이스를 해제하지 않고 로컬 상태 디렉토리를 삭제하지 마십시오. 새로 시작해야 하는 경우, 항상 먼저 디바이스를 해제하십시오.

### I. 복구 키 (E2EE에 권장)

복구 키를 사용하면 ZeroClaw가 서버 측 백업에서 방 키와 크로스 서명 시크릿을 자동으로 복원할 수 있습니다. 즉, 디바이스 초기화, 암호화 저장소 삭제, 새 설치 시에도 자동으로 복구됩니다 — 이모지 검증이나 수동 키 공유가 필요 없습니다.

#### 1단계: Element에서 복구 키 가져오기

1. Element(웹 또는 데스크톱)에서 봇 계정으로 로그인합니다
2. 설정 → 보안 및 개인정보 → 암호화 → 보안 백업으로 이동합니다
3. 백업이 이미 설정된 경우, 처음 활성화할 때 복구 키가 표시되었습니다. 저장해 두었다면 그것을 사용합니다.
4. 백업이 설정되지 않은 경우, "보안 백업 설정"을 클릭하고 "보안 키 생성"을 선택합니다. 키를 저장합니다 — `EsTj 3yST y93F SLpB ...` 형식입니다
5. 완료 후 Element에서 로그아웃합니다

#### 2단계: ZeroClaw에 복구 키 추가

옵션 A — 온보딩 중:

```bash
zeroclaw onboard
# 또는
zeroclaw onboard --channels-only
```

Matrix 채널 설정 시 마법사에서 다음과 같이 묻습니다:

```
E2EE recovery key (or Enter to skip): EsTj 3yST y93F SLpB jJsz ...
```

복구 키를 붙여넣습니다 (입력은 마스킹됩니다). 암호화되어 `config.toml`에 `channels_config.matrix.recovery_key`로 저장됩니다.

옵션 B — 시크릿 CLI를 통해 (기존 설치에 권장):

```bash
zeroclaw props set channels.matrix.recovery-key
```

입력은 마스킹됩니다. 값은 즉시 암호화되어 저장됩니다.

옵션 C — `config.toml`을 직접 편집:

```toml
[channels_config.matrix]
recovery_key = "EsTj 3yST y93F SLpB jJsz ..."
```

`secrets.encrypt = true` (기본값)인 경우, 다음 설정 저장 시 값이 암호화됩니다. 참고: 저장이 트리거될 때까지 값은 평문으로 남아 있습니다. 옵션 A 또는 B를 사용하는 것이 좋습니다.

#### 3단계: ZeroClaw 재시작

시작 시 다음이 표시되어야 합니다:

```
Matrix E2EE recovery successful — room keys and cross-signing secrets restored from server backup.
```

이제부터 로컬 암호화 저장소가 삭제되더라도 ZeroClaw는 다음 시작 시 자동으로 복구됩니다.

---

## 5. 디버그 로깅

상세한 E2EE 진단을 위해 Matrix 채널에 대해 debug 수준 로깅으로 ZeroClaw를 실행합니다:

```bash
RUST_LOG=zeroclaw::channels::matrix=debug zeroclaw daemon
```

다음 정보가 표시됩니다:
- 세션 복원 확인
- 각 sync 주기 완료
- OTK 충돌 플래그 상태
- 헬스 체크 결과
- 일시적 vs. 치명적 sync 오류 분류

Matrix SDK 자체에서 더 자세한 정보를 보려면:

```bash
RUST_LOG=zeroclaw::channels::matrix=debug,matrix_sdk_crypto=debug zeroclaw daemon
```

---

## 6. 운영 참고 사항

- Matrix 토큰을 로그와 스크린샷에 노출하지 마십시오.
- 허용적인 `allowed_users`로 시작한 후 명시적 사용자 ID로 범위를 좁히십시오.
- 프로덕션에서는 별칭 변동을 피하기 위해 정규 방 ID를 사용하십시오.
- **스레딩 동작:** ZeroClaw는 항상 사용자의 원본 메시지를 루트로 하는 스레드에서 응답합니다. 각 스레드는 자체적으로 격리된 대화 컨텍스트를 유지합니다. 메인 방 타임라인은 영향을 받지 않으며 — 스레드는 서로 간에 또는 방과 컨텍스트를 공유하지 않습니다. 암호화된 방에서도 스레딩은 동일하게 작동합니다 — SDK가 스레드 컨텍스트를 평가하기 전에 이벤트를 투명하게 복호화합니다.

---

## 7. 관련 문서

- [채널 레퍼런스](../reference/api/channels-reference.md)
- [운영 로그 키워드 부록](../reference/api/channels-reference.md#7-operations-appendix-log-keywords-matrix)
- [네트워크 배포](../ops/network-deployment.md)
- [플랫폼 무관 보안](./agnostic-security.md)
- [리뷰어 플레이북](../contributing/reviewer-playbook.md)
