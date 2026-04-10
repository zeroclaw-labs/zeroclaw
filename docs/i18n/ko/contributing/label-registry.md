# 라벨 레지스트리

PR 및 이슈에 사용되는 모든 라벨에 대한 단일 참조입니다. 라벨은 카테고리별로 그룹화되어 있습니다. 각 항목은 라벨 이름, 정의, 적용 방법을 나열합니다.

여기에 통합된 소스:

- `.github/labeler.yml` (`actions/labeler`용 경로 라벨 설정)
- `.github/label-policy.json` (기여자 등급 임계값)
- `docs/contributing/pr-workflow.md` (크기, 리스크, 분류 라벨 정의)
- `docs/contributing/ci-map.md` (자동화 동작 및 고위험 경로 휴리스틱)

참고: CI가 4개 워크플로우(`ci.yml`, `release.yml`, `ci-full.yml`, `promote-release.yml`)로 간소화되었습니다. 이전에 크기, 리스크, 기여자 등급, 분류 라벨을 자동화했던 워크플로우(`pr-labeler.yml`, `pr-auto-response.yml`, `pr-check-stale.yml` 및 지원 스크립트)가 제거되었습니다. 현재는 `pr-path-labeler.yml`을 통한 경로 라벨만 자동화됩니다.

---

## 경로 라벨

`actions/labeler`를 사용하는 `pr-path-labeler.yml`에 의해 자동으로 적용됩니다. `.github/labeler.yml`의 glob 패턴에 대해 변경된 파일을 매칭합니다.

### 기본 범위 라벨

| 라벨 | 매칭 |
|---|---|
| `docs` | `docs/**`, `**/*.md`, `**/*.mdx`, `LICENSE`, `.markdownlint-cli2.yaml` |
| `dependencies` | `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.github/dependabot.yml` |
| `ci` | `.github/**`, `.githooks/**` |
| `core` | `src/*.rs` |
| `agent` | `src/agent/**` |
| `channel` | `src/channels/**` |
| `gateway` | `src/gateway/**` |
| `config` | `src/config/**` |
| `cron` | `src/cron/**` |
| `daemon` | `src/daemon/**` |
| `doctor` | `src/doctor/**` |
| `health` | `src/health/**` |
| `heartbeat` | `src/heartbeat/**` |
| `integration` | `src/integrations/**` |
| `memory` | `src/memory/**` |
| `security` | `src/security/**` |
| `runtime` | `src/runtime/**` |
| `onboard` | `src/onboard/**` |
| `provider` | `src/providers/**` |
| `service` | `src/service/**` |
| `skillforge` | `src/skillforge/**` |
| `skills` | `src/skills/**` |
| `tool` | `src/tools/**` |
| `tunnel` | `src/tunnel/**` |
| `observability` | `src/observability/**` |
| `tests` | `tests/**` |
| `scripts` | `scripts/**` |
| `dev` | `dev/**` |

### 컴포넌트별 채널 라벨

각 채널은 기본 `channel` 라벨에 더해 특정 라벨을 받습니다.

| 라벨 | 매칭 |
|---|---|
| `channel:bluesky` | `bluesky.rs` |
| `channel:clawdtalk` | `clawdtalk.rs` |
| `channel:cli` | `cli.rs` |
| `channel:dingtalk` | `dingtalk.rs` |
| `channel:discord` | `discord.rs`, `discord_history.rs` |
| `channel:email` | `email_channel.rs`, `gmail_push.rs` |
| `channel:imessage` | `imessage.rs` |
| `channel:irc` | `irc.rs` |
| `channel:lark` | `lark.rs` |
| `channel:linq` | `linq.rs` |
| `channel:matrix` | `matrix.rs` |
| `channel:mattermost` | `mattermost.rs` |
| `channel:mochat` | `mochat.rs` |
| `channel:mqtt` | `mqtt.rs` |
| `channel:nextcloud-talk` | `nextcloud_talk.rs` |
| `channel:nostr` | `nostr.rs` |
| `channel:notion` | `notion.rs` |
| `channel:qq` | `qq.rs` |
| `channel:reddit` | `reddit.rs` |
| `channel:signal` | `signal.rs` |
| `channel:slack` | `slack.rs` |
| `channel:telegram` | `telegram.rs` |
| `channel:twitter` | `twitter.rs` |
| `channel:wati` | `wati.rs` |
| `channel:webhook` | `webhook.rs` |
| `channel:wecom` | `wecom.rs` |
| `channel:whatsapp` | `whatsapp.rs`, `whatsapp_storage.rs`, `whatsapp_web.rs` |

### 컴포넌트별 프로바이더 라벨

| 라벨 | 매칭 |
|---|---|
| `provider:anthropic` | `anthropic.rs` |
| `provider:azure-openai` | `azure_openai.rs` |
| `provider:bedrock` | `bedrock.rs` |
| `provider:claude-code` | `claude_code.rs` |
| `provider:compatible` | `compatible.rs` |
| `provider:copilot` | `copilot.rs` |
| `provider:gemini` | `gemini.rs`, `gemini_cli.rs` |
| `provider:glm` | `glm.rs` |
| `provider:kilocli` | `kilocli.rs` |
| `provider:ollama` | `ollama.rs` |
| `provider:openai` | `openai.rs`, `openai_codex.rs` |
| `provider:openrouter` | `openrouter.rs` |
| `provider:telnyx` | `telnyx.rs` |

### 그룹별 도구 라벨

도구는 파일당 하나의 라벨이 아닌 논리적 기능별로 그룹화됩니다.

| 라벨 | 매칭 |
|---|---|
| `tool:browser` | `browser.rs`, `browser_delegate.rs`, `browser_open.rs`, `text_browser.rs`, `screenshot.rs` |
| `tool:cloud` | `cloud_ops.rs`, `cloud_patterns.rs` |
| `tool:composio` | `composio.rs` |
| `tool:cron` | `cron_add.rs`, `cron_list.rs`, `cron_remove.rs`, `cron_run.rs`, `cron_runs.rs`, `cron_update.rs` |
| `tool:file` | `file_edit.rs`, `file_read.rs`, `file_write.rs`, `glob_search.rs`, `content_search.rs` |
| `tool:google-workspace` | `google_workspace.rs` |
| `tool:mcp` | `mcp_client.rs`, `mcp_deferred.rs`, `mcp_protocol.rs`, `mcp_tool.rs`, `mcp_transport.rs` |
| `tool:memory` | `memory_forget.rs`, `memory_recall.rs`, `memory_store.rs` |
| `tool:microsoft365` | `microsoft365/**` |
| `tool:security` | `security_ops.rs`, `verifiable_intent.rs` |
| `tool:shell` | `shell.rs`, `node_tool.rs`, `cli_discovery.rs` |
| `tool:sop` | `sop_advance.rs`, `sop_approve.rs`, `sop_execute.rs`, `sop_list.rs`, `sop_status.rs` |
| `tool:web` | `web_fetch.rs`, `web_search_tool.rs`, `web_search_provider_routing.rs`, `http_request.rs` |

---

## 크기 라벨

`pr-workflow.md` 6.1절에 정의되어 있습니다. 유효 변경 라인 수 기반이며, 문서 전용 및 lockfile 중심 PR에 대해 정규화됩니다.

| 라벨 | 임계값 |
|---|---|
| `size: XS` | <= 80 라인 |
| `size: S` | <= 250 라인 |
| `size: M` | <= 500 라인 |
| `size: L` | <= 1000 라인 |
| `size: XL` | > 1000 라인 |

**적용 주체:** 수동. 이전에 크기 라벨을 계산하던 워크플로우(`pr-labeler.yml` 및 지원 스크립트)는 CI 간소화 중 제거되었습니다.

---

## 리스크 라벨

`pr-workflow.md` 13.2절과 `ci-map.md`에 정의되어 있습니다. 변경된 경로와 변경 크기를 결합한 휴리스틱 기반입니다.

| 라벨 | 의미 |
|---|---|
| `risk: low` | 고위험 경로가 변경되지 않음, 작은 변경 |
| `risk: medium` | 경계/보안 영향 없는 `src/**`의 동작 변경 |
| `risk: high` | 고위험 경로(아래 참조)를 변경하거나 보안 관련 큰 변경 |
| `risk: manual` | 자동 리스크 재계산을 동결하는 메인테이너 오버라이드 |

고위험 경로: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`.

low와 medium의 경계는 "고위험 경로 없음" 이상으로 공식적으로 정의되지 않습니다.

**적용 주체:** 수동. 이전에 `pr-labeler.yml`을 통해 자동화되었으나 CI 간소화 중 제거되었습니다.

---

## 기여자 등급 라벨

`.github/label-policy.json`에 정의되어 있습니다. GitHub API에서 조회한 작성자의 merge된 PR 수 기반입니다.

| 라벨 | 최소 merge된 PR |
|---|---|
| `trusted contributor` | 5 |
| `experienced contributor` | 10 |
| `principal contributor` | 20 |
| `distinguished contributor` | 50 |

**적용 주체:** 수동. 이전에 `pr-labeler.yml`과 `pr-auto-response.yml`을 통해 자동화되었으나 CI 간소화 중 제거되었습니다.

---

## 응답 및 분류 라벨

`pr-workflow.md` 8절에 정의되어 있습니다. 수동으로 적용됩니다.

| 라벨 | 목적 | 적용 주체 |
|---|---|---|
| `r:needs-repro` | 불완전한 버그 리포트; 결정적 재현 요청 | 수동 |
| `r:support` | 버그 백로그 외부에서 처리하는 것이 나은 사용법/도움 항목 | 수동 |
| `invalid` | 유효하지 않은 버그/기능 요청 | 수동 |
| `duplicate` | 기존 이슈의 중복 | 수동 |
| `stale-candidate` | 휴면 PR/이슈; 닫기 후보 | 수동 |
| `superseded` | 더 새로운 PR로 대체됨 | 수동 |
| `no-stale` | stale 자동화에서 제외; 수락되었지만 차단된 작업 | 수동 |

**자동화:** 현재 없음. 라벨 기반 이슈 닫기(`pr-auto-response.yml`)와 stale 감지(`pr-check-stale.yml`)를 처리하던 워크플로우는 CI 간소화 중 제거되었습니다.

---

## 구현 상태

| 카테고리 | 개수 | 자동화 | 워크플로우 |
|---|---|---|---|
| 경로 (기본 범위) | 27 | 예 | `pr-path-labeler.yml` |
| 경로 (컴포넌트별) | 52 | 예 | `pr-path-labeler.yml` |
| 크기 | 5 | 아니오 | 수동 |
| 리스크 | 4 | 아니오 | 수동 |
| 기여자 등급 | 4 | 아니오 | 수동 |
| 응답/분류 | 7 | 아니오 | 수동 |
| **합계** | **99** | | |

---

## 유지보수

- **소유자:** 라벨 정책 및 PR 분류 자동화를 담당하는 메인테이너.
- **업데이트 트리거:** 소스 트리에 새 채널, 프로바이더 또는 도구가 추가될 때; 라벨 정책 변경; 분류 워크플로우 변경.
- **정보 출처:** 이 문서는 상단에 나열된 네 개의 소스 파일에서 정의를 통합합니다. 정의가 충돌할 경우, 먼저 소스 파일을 업데이트한 다음 이 레지스트리를 동기화합니다.
