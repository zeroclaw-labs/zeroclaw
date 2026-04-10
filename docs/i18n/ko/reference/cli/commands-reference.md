# ZeroClaw 명령어 레퍼런스

이 레퍼런스는 현재 CLI 인터페이스(`zeroclaw --help`)를 기반으로 작성되었습니다.

최종 검증일: **2026년 3월 26일**.

## 최상위 명령어

| 명령어 | 용도 |
|---|---|
| `onboard` | 워크스페이스/config를 빠르게 또는 대화형으로 초기화합니다 |
| `agent` | 대화형 채팅 또는 단일 메시지 모드를 실행합니다 |
| `gateway` | webhook 및 WhatsApp HTTP gateway를 시작합니다 |
| `acp` | stdio를 통한 ACP (Agent Control Protocol) 서버를 시작합니다 |
| `daemon` | 감독형 런타임(gateway + channel + 선택적 heartbeat/scheduler)을 시작합니다 |
| `service` | 사용자 수준 OS 서비스 라이프사이클을 관리합니다 |
| `doctor` | 진단 및 최신 상태 검사를 수행합니다 |
| `status` | 현재 구성 및 시스템 요약을 출력합니다 |
| `estop` | 비상 정지 레벨을 활성화/해제하고 estop 상태를 조회합니다 |
| `cron` | 예약 작업을 관리합니다 |
| `models` | provider 모델 카탈로그를 갱신합니다 |
| `providers` | provider ID, 별칭, 활성 provider를 조회합니다 |
| `channel` | channel을 관리하고 channel 상태를 점검합니다 |
| `integrations` | 통합 상세 정보를 조회합니다 |
| `skills` | skill을 조회/설치/제거합니다 |
| `migrate` | 외부 런타임(현재 OpenClaw)에서 가져옵니다 |
| `props` | config 속성을 조회, 설정 또는 초기화합니다 |
| `config` | 기계 판독 가능한 config 스키마를 내보냅니다 |
| `completions` | 셸 자동 완성 스크립트를 stdout으로 생성합니다 |
| `hardware` | USB 하드웨어를 검색하고 상세 조회합니다 |
| `peripheral` | 주변 장치를 구성하고 펌웨어를 플래시합니다 |

## 명령어 그룹

### `onboard`

- `zeroclaw onboard`
- `zeroclaw onboard --channels-only`
- `zeroclaw onboard --force`
- `zeroclaw onboard --reinit`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` 안전 동작:

- `config.toml`이 이미 존재하면 온보딩은 두 가지 모드를 제공합니다:
  - 전체 온보딩(`config.toml` 덮어쓰기)
  - provider만 업데이트(기존 channel, tunnel, memory, hook 및 기타 설정을 유지하면서 provider/model/API key만 업데이트)
- 비대화형 환경에서는 기존 `config.toml`이 있으면 `--force`를 전달하지 않는 한 안전하게 거부합니다.
- channel 토큰/allowlist만 교체하려면 `zeroclaw onboard --channels-only`를 사용하십시오.
- `zeroclaw onboard --reinit`을 사용하면 처음부터 다시 시작합니다. 기존 config 디렉터리를 타임스탬프 접미사로 백업한 후 새 구성을 처음부터 생성합니다.

### `agent`

- `zeroclaw agent`
- `zeroclaw agent -m "Hello"`
- `zeroclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `zeroclaw agent --peripheral <board:path>`

팁:

- 대화형 채팅에서 자연어로 라우트 변경을 요청할 수 있습니다(예: "conversation uses kimi, coding uses gpt-5.3-codex"). 어시스턴트가 `model_routing_config` 도구를 통해 이를 저장할 수 있습니다.

### `acp`

- `zeroclaw acp`
- `zeroclaw acp --max-sessions <N>`
- `zeroclaw acp --session-timeout <SECONDS>`

IDE 및 도구 통합을 위한 ACP (Agent Control Protocol) 서버를 시작합니다.

- stdin/stdout을 통한 JSON-RPC 2.0 사용
- 지원 메서드: `initialize`, `session/new`, `session/prompt`, `session/stop`
- agent 추론, 도구 호출, 콘텐츠를 실시간 알림으로 스트리밍
- 기본 최대 세션 수: 10
- 기본 세션 타임아웃: 3600초 (1시간)

### `gateway` / `daemon`

- `zeroclaw gateway [--host <HOST>] [--port <PORT>]`
- `zeroclaw daemon [--host <HOST>] [--port <PORT>]`

### `estop`

- `zeroclaw estop` (`kill-all` 활성화)
- `zeroclaw estop --level network-kill`
- `zeroclaw estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `zeroclaw estop --level tool-freeze --tool shell [--tool browser]`
- `zeroclaw estop status`
- `zeroclaw estop resume`
- `zeroclaw estop resume --network`
- `zeroclaw estop resume --domain "*.chase.com"`
- `zeroclaw estop resume --tool shell`
- `zeroclaw estop resume --otp <123456>`

참고:

- `estop` 명령어는 `[security.estop].enabled = true`가 필요합니다.
- `[security.estop].require_otp_to_resume = true`인 경우 `resume`에 OTP 검증이 필요합니다.
- `--otp`를 생략하면 OTP 프롬프트가 자동으로 표시됩니다.

### `service`

- `zeroclaw service install`
- `zeroclaw service start`
- `zeroclaw service stop`
- `zeroclaw service restart`
- `zeroclaw service status`
- `zeroclaw service uninstall`

### `cron`

- `zeroclaw cron list`
- `zeroclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `zeroclaw cron add-at <rfc3339_timestamp> <command>`
- `zeroclaw cron add-every <every_ms> <command>`
- `zeroclaw cron once <delay> <command>`
- `zeroclaw cron remove <id>`
- `zeroclaw cron pause <id>`
- `zeroclaw cron resume <id>`

참고:

- 스케줄/cron 변경 작업은 `cron.enabled = true`가 필요합니다.
- 스케줄 생성(`create` / `add` / `once`)의 셸 명령 페이로드는 작업 저장 전에 보안 명령 정책에 의해 검증됩니다.

### `models`

- `zeroclaw models refresh`
- `zeroclaw models refresh --provider <ID>`
- `zeroclaw models refresh --force`

`models refresh`는 현재 다음 provider ID에 대해 실시간 카탈로그 갱신을 지원합니다: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `llamacpp`, `sglang`, `vllm`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen`, `nvidia`.

### `doctor`

- `zeroclaw doctor`
- `zeroclaw doctor models [--provider <ID>] [--use-cache]`
- `zeroclaw doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `zeroclaw doctor traces --id <TRACE_ID>`

`doctor traces`는 `observability.runtime_trace_path`에서 런타임 도구/모델 진단 정보를 읽습니다.

### `channel`

- `zeroclaw channel list`
- `zeroclaw channel start`
- `zeroclaw channel doctor`
- `zeroclaw channel bind-telegram <IDENTITY>`
- `zeroclaw channel add <type> <json>`
- `zeroclaw channel remove <name>`

런타임 채팅 내 명령어(channel 서버가 실행 중일 때 Telegram/Discord에서 사용):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`
- `/new`

channel 런타임은 `config.toml`을 감시하며 다음 항목의 변경 사항을 핫 적용합니다:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (기본 provider용)
- `reliability.*` provider 재시도 설정

`add/remove`는 현재 관리형 설정/수동 config 경로로 안내합니다(아직 완전한 선언적 변경 기능은 아닙니다).

### `integrations`

- `zeroclaw integrations info <name>`

### `skills`

- `zeroclaw skills list`
- `zeroclaw skills audit <source_or_name>`
- `zeroclaw skills install <source>`
- `zeroclaw skills remove <name>`

`<source>`는 git 원격 저장소(`https://...`, `http://...`, `ssh://...`, `git@host:owner/repo.git`) 또는 로컬 파일 시스템 경로를 허용합니다.

`skills install`은 skill이 수락되기 전에 항상 내장 정적 보안 감사를 수행합니다. 감사에서 차단하는 항목:
- skill 패키지 내 심볼릭 링크
- 스크립트 유형 파일(`.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`)
- 고위험 명령 스니펫(예: 파이프-투-셸 페이로드)
- skill 루트를 벗어나거나, 원격 마크다운을 가리키거나, 스크립트 파일을 대상으로 하는 마크다운 링크

`skills audit`를 사용하여 후보 skill 디렉터리(또는 이름으로 설치된 skill)를 공유 전에 수동으로 검증할 수 있습니다.

Skill 매니페스트(`SKILL.toml`)는 `prompts`와 `[[tools]]`를 지원합니다. 이 두 가지 모두 런타임에 agent 시스템 프롬프트에 주입되므로, 모델이 skill 파일을 수동으로 읽지 않아도 skill 지시사항을 따를 수 있습니다.

### `migrate`

- `zeroclaw migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `zeroclaw config schema`

`config schema`는 전체 `config.toml` 계약에 대한 JSON Schema(draft 2020-12)를 stdout으로 출력합니다.

### `completions`

- `zeroclaw completions bash`
- `zeroclaw completions fish`
- `zeroclaw completions zsh`
- `zeroclaw completions powershell`
- `zeroclaw completions elvish`

`completions`는 스크립트를 로그/경고 오염 없이 직접 소싱할 수 있도록 stdout 전용으로 설계되었습니다.

### `hardware`

- `zeroclaw hardware discover`
- `zeroclaw hardware introspect <path>`
- `zeroclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `zeroclaw peripheral list`
- `zeroclaw peripheral add <board> <path>`
- `zeroclaw peripheral flash [--port <serial_port>]`
- `zeroclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `zeroclaw peripheral flash-nucleo`

### `props`

`config.toml`을 직접 편집하지 않고 개별 config 속성을 관리합니다.
속성은 점으로 구분된 경로로 참조합니다(예: `channels.matrix.mention-only`).

- `zeroclaw props list` — 모든 속성과 현재 값을 나열합니다
- `zeroclaw props list --secrets` — 비밀(암호화된) 필드만 나열합니다
- `zeroclaw props list --filter channels.matrix` — 경로 접두사로 필터링합니다
- `zeroclaw props get <path>` — 단일 속성 값을 조회합니다(비밀 필드는 설정/미설정 상태를 표시)
- `zeroclaw props set <path> <value>` — 속성 값을 설정합니다
- `zeroclaw props set <path>` — 비밀 필드는 마스킹된 입력을 요청하고, enum 필드는 대화형 선택을 제공합니다
- `zeroclaw props set --no-interactive <path> <value>` — 스크립트 모드, 프롬프트 없음
- `zeroclaw props init <section>` — 기본값으로 미구성 섹션을 생성합니다(`enabled=false`)
- `zeroclaw props init` — 모든 미구성 섹션을 초기화합니다

비밀 필드(API key, 토큰, 비밀번호)는 `#[secret]` 어노테이션을 통해 자동으로 감지됩니다.
비밀을 설정할 때 명령줄에서 값을 제공했는지 여부와 관계없이 입력이 마스킹됩니다.

enum 필드(예: `stream-mode`, `search-mode`)는 값을 생략하면 화살표 키를 통한 대화형 선택을 제공합니다.
프롬프트를 건너뛰려면 값을 직접 입력하십시오.

속성 경로에 대한 셸 탭 완성은 `zeroclaw completions <shell>`에 포함되어 있습니다.

#### 새 config 필드 추가

Config 구조체는 `#[prefix]`와 `#[nested]` 속성을 사용하여 `Configurable`을 파생합니다.
기존 구조체에 새 필드를 추가하면 즉시 `props`를 통해 사용할 수 있습니다.
새 enum 타입은 한 줄의 `HasPropKind` 구현이 필요합니다. 자세한 내용은 `CONTRIBUTING.md`를 참조하십시오.

## 검증 팁

현재 바이너리에 대해 문서를 빠르게 검증하려면:

```bash
zeroclaw --help
zeroclaw <command> --help
```
