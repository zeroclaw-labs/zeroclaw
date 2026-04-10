# Proxy Agent 플레이북

이 플레이북은 `proxy_config`를 통한 proxy 동작 구성을 위한 복사-붙여넣기 가능한 tool call을 제공합니다.

에이전트의 proxy 범위를 빠르고 안전하게 전환하려면 이 문서를 사용하십시오.

## 0. 요약

- **목적:** proxy 범위 관리 및 롤백을 위한 복사 가능한 에이전트 tool call 제공.
- **대상:** 프록시 네트워크에서 ZeroClaw를 운영하는 운영자 및 유지보수 담당자.
- **범위:** `proxy_config` 액션, 모드 선택, 검증 흐름 및 문제 해결.
- **비범위:** ZeroClaw 런타임 동작 외의 일반적인 네트워크 디버깅.

---

## 1. 의도별 빠른 경로

빠른 운영 라우팅을 위해 이 섹션을 사용하십시오.

### 1.1 ZeroClaw 내부 트래픽만 프록시

1. 범위를 `zeroclaw`로 설정합니다.
2. `http_proxy`/`https_proxy` 또는 `all_proxy`를 설정합니다.
3. `{"action":"get"}`으로 검증합니다.

이동:

- [섹션 4](#4-모드-a--zeroclaw-내부-전용-proxy)

### 1.2 선택한 서비스만 프록시

1. 범위를 `services`로 설정합니다.
2. `services`에 구체적인 키 또는 와일드카드 셀렉터를 설정합니다.
3. `{"action":"list_services"}`로 범위를 검증합니다.

이동:

- [섹션 5](#5-모드-b--특정-서비스-전용-proxy)

### 1.3 프로세스 전체 proxy 환경 변수 내보내기

1. 범위를 `environment`로 설정합니다.
2. `{"action":"apply_env"}`로 적용합니다.
3. `{"action":"get"}`으로 환경 스냅샷을 확인합니다.

이동:

- [섹션 6](#6-모드-c--전체-프로세스-환경-proxy)

### 1.4 긴급 롤백

1. proxy를 비활성화합니다.
2. 필요한 경우 환경 변수 내보내기를 지웁니다.
3. 런타임 및 환경 스냅샷을 다시 확인합니다.

이동:

- [섹션 7](#7-비활성화--롤백-패턴)

---

## 2. 범위 결정 매트릭스

| 범위 | 영향 대상 | 환경 변수 내보내기 | 일반적인 용도 |
|---|---|---|---|
| `zeroclaw` | ZeroClaw 내부 HTTP 클라이언트 | 아니오 | 프로세스 수준 부작용 없이 일반적인 런타임 프록시 |
| `services` | 선택된 서비스 키/셀렉터만 | 아니오 | 특정 provider/tool/채널에 대한 세밀한 라우팅 |
| `environment` | 런타임 + 프로세스 환경 proxy 변수 | 예 | `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`가 필요한 통합 |

---

## 3. 표준 안전 워크플로우

모든 proxy 변경에 대해 다음 순서를 사용하십시오:

1. 현재 상태를 확인합니다.
2. 유효한 서비스 키/셀렉터를 탐색합니다.
3. 대상 범위 설정을 적용합니다.
4. 런타임 및 환경 스냅샷을 검증합니다.
5. 동작이 예상과 다르면 롤백합니다.

Tool call:

```json
{"action":"get"}
{"action":"list_services"}
```

---

## 4. 모드 A — ZeroClaw 내부 전용 Proxy

ZeroClaw provider/채널/tool HTTP 트래픽에 proxy를 사용하되, 프로세스 수준 proxy 환경 변수를 내보내지 않을 때 사용합니다.

Tool call:

```json
{"action":"set","enabled":true,"scope":"zeroclaw","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":["localhost","127.0.0.1"]}
{"action":"get"}
```

예상 동작:

- ZeroClaw HTTP 클라이언트에 대해 런타임 proxy가 활성화됩니다.
- `HTTP_PROXY` / `HTTPS_PROXY` 프로세스 환경 변수 내보내기는 필요하지 않습니다.

---

## 5. 모드 B — 특정 서비스 전용 Proxy

시스템의 일부만 proxy를 사용해야 할 때 사용합니다 (예: 특정 provider/tool/채널).

### 5.1 특정 서비스 대상

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.http_request","channel.telegram"],"all_proxy":"socks5h://127.0.0.1:1080","no_proxy":["localhost","127.0.0.1",".internal"]}
{"action":"get"}
```

### 5.2 셀렉터로 대상 지정

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.*","tool.*"],"http_proxy":"http://127.0.0.1:7890"}
{"action":"get"}
```

예상 동작:

- 일치하는 서비스만 proxy를 사용합니다.
- 일치하지 않는 서비스는 proxy를 우회합니다.

---

## 6. 모드 C — 전체 프로세스 환경 Proxy

런타임 통합을 위해 내보낸 프로세스 환경 변수 (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`)가 의도적으로 필요한 경우 사용합니다.

### 6.1 environment 범위 구성 및 적용

```json
{"action":"set","enabled":true,"scope":"environment","http_proxy":"http://127.0.0.1:7890","https_proxy":"http://127.0.0.1:7890","no_proxy":"localhost,127.0.0.1,.internal"}
{"action":"apply_env"}
{"action":"get"}
```

예상 동작:

- 런타임 proxy가 활성화됩니다.
- 프로세스에 대해 환경 변수가 내보내집니다.

---

## 7. 비활성화 / 롤백 패턴

### 7.1 proxy 비활성화 (기본 안전 동작)

```json
{"action":"disable"}
{"action":"get"}
```

### 7.2 proxy 비활성화 및 환경 변수 강제 제거

```json
{"action":"disable","clear_env":true}
{"action":"get"}
```

### 7.3 proxy는 활성 상태로 유지하고 환경 변수 내보내기만 제거

```json
{"action":"clear_env"}
{"action":"get"}
```

---

## 8. 일반적인 운영 레시피

### 8.1 환경 전체 proxy에서 서비스 전용 proxy로 전환

```json
{"action":"set","enabled":true,"scope":"services","services":["provider.openai","tool.http_request"],"all_proxy":"socks5://127.0.0.1:1080"}
{"action":"get"}
```

### 8.2 프록시 대상 서비스 추가

```json
{"action":"set","scope":"services","services":["provider.openai","tool.http_request","channel.slack"]}
{"action":"get"}
```

### 8.3 셀렉터로 `services` 목록 재설정

```json
{"action":"set","scope":"services","services":["provider.*","channel.telegram"]}
{"action":"get"}
```

---

## 9. 문제 해결

- 오류: `proxy.scope='services' requires a non-empty proxy.services list`
  - 해결: 최소 하나의 구체적인 서비스 키 또는 셀렉터를 설정하십시오.

- 오류: 잘못된 proxy URL 스키마
  - 허용되는 스키마: `http`, `https`, `socks5`, `socks5h`.

- proxy가 예상대로 적용되지 않는 경우
  - `{"action":"list_services"}`를 실행하여 서비스 이름/셀렉터를 확인하십시오.
  - `{"action":"get"}`을 실행하여 `runtime_proxy` 및 `environment` 스냅샷 값을 확인하십시오.

---

## 10. 관련 문서

- [README.md](./README.md) — 문서 색인 및 분류 체계.
- [network-deployment.md](./network-deployment.md) — 엔드투엔드 네트워크 deployment 및 터널 토폴로지 안내.
- [resource-limits.md](./resource-limits.md) — 네트워크/tool 실행 컨텍스트의 런타임 안전 제한.

---

## 11. 유지보수 참고 사항

- **담당:** 런타임 및 도구 유지보수 담당자.
- **업데이트 트리거:** 새로운 `proxy_config` 액션, proxy 범위 의미 변경 또는 지원되는 서비스 셀렉터 변경.
- **마지막 검토일:** 2026-02-18.
