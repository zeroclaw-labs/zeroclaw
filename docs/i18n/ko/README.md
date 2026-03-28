# ZeroClaw 문서 허브

이 페이지는 문서 시스템의 기본 진입점입니다.

마지막 업데이트: **2026년 2월 21일**.

현지화된 허브: [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Tiếng Việt](i18n/vi/README.md).

## 여기서 시작하세요

| 하고 싶은 것…                                                       | 이것을 읽으세요                                                                |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| ZeroClaw를 빠르게 설치하고 실행                                     | [README.md (빠른 시작)](../README.md#quick-start)                              |
| 한 번의 명령으로 부트스트랩                                         | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md)                  |
| macOS에서 업데이트 또는 제거                                        | [macos-update-uninstall.md](setup-guides/macos-update-uninstall.md)            |
| 작업별 명령어 찾기                                                  | [commands-reference.md](reference/cli/commands-reference.md)                   |
| 구성 기본값과 키를 빠르게 확인                                      | [config-reference.md](reference/api/config-reference.md)                       |
| 사용자 정의 프로바이더/엔드포인트 구성                              | [custom-providers.md](contributing/custom-providers.md)                        |
| Z.AI / GLM 프로바이더 구성                                          | [zai-glm-setup.md](setup-guides/zai-glm-setup.md)                             |
| LangGraph 통합 패턴 사용                                            | [langgraph-integration.md](contributing/langgraph-integration.md)              |
| 런타임 운영 (2일차 런북)                                            | [operations-runbook.md](ops/operations-runbook.md)                             |
| 설치/런타임/채널 문제 해결                                          | [troubleshooting.md](ops/troubleshooting.md)                                   |
| Matrix 암호화 방 설정 및 진단 실행                                  | [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md)                          |
| 카테고리별 문서 찾아보기                                            | [SUMMARY.md](SUMMARY.md)                                                      |
| 프로젝트 PR/이슈 문서 스냅샷 보기                                   | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## 빠른 의사결정 트리 (10초)

- 초기 설정 또는 설치가 필요한가요? → [setup-guides/README.md](setup-guides/README.md)
- 정확한 CLI/구성 키가 필요한가요? → [reference/README.md](reference/README.md)
- 프로덕션/서비스 운영이 필요한가요? → [ops/README.md](ops/README.md)
- 실패 또는 회귀가 발생하고 있나요? → [troubleshooting.md](ops/troubleshooting.md)
- 보안 강화 또는 로드맵 작업 중인가요? → [security/README.md](security/README.md)
- 보드/주변 장치 작업 중인가요? → [hardware/README.md](hardware/README.md)
- 기여/검토/CI 워크플로우? → [contributing/README.md](contributing/README.md)
- 전체 맵이 필요한가요? → [SUMMARY.md](SUMMARY.md)

## 컬렉션 (권장)

- 시작하기: [setup-guides/README.md](setup-guides/README.md)
- 참조 카탈로그: [reference/README.md](reference/README.md)
- 운영 및 배포: [ops/README.md](ops/README.md)
- 보안 문서: [security/README.md](security/README.md)
- 하드웨어/주변 장치: [hardware/README.md](hardware/README.md)
- 기여/CI: [contributing/README.md](contributing/README.md)
- 프로젝트 스냅샷: [maintainers/README.md](maintainers/README.md)

## 대상별

### 사용자 / 운영자

- [commands-reference.md](reference/cli/commands-reference.md) — 워크플로우별 명령어 검색
- [providers-reference.md](reference/api/providers-reference.md) — 프로바이더 ID, 별칭, 자격 증명 환경 변수
- [channels-reference.md](reference/api/channels-reference.md) — 채널 기능 및 설정 경로
- [matrix-e2ee-guide.md](security/matrix-e2ee-guide.md) — Matrix 암호화 방(E2EE) 설정 및 무응답 진단
- [config-reference.md](reference/api/config-reference.md) — 주요 구성 키 및 보안 기본값
- [custom-providers.md](contributing/custom-providers.md) — 사용자 정의 프로바이더/기본 URL 통합 템플릿
- [zai-glm-setup.md](setup-guides/zai-glm-setup.md) — Z.AI/GLM 설정 및 엔드포인트 매트릭스
- [langgraph-integration.md](contributing/langgraph-integration.md) — 모델/도구 호출 엣지 케이스를 위한 폴백 통합
- [operations-runbook.md](ops/operations-runbook.md) — 2일차 런타임 운영 및 롤백 흐름
- [troubleshooting.md](ops/troubleshooting.md) — 일반적인 실패 시그니처 및 복구 단계

### 기여자 / 유지보수자

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### 보안 / 신뢰성

> 참고: 이 영역에는 제안/로드맵 문서가 포함되어 있습니다. 현재 동작에 대해서는 [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), [troubleshooting.md](ops/troubleshooting.md)를 먼저 참조하세요.

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## 시스템 탐색 및 거버넌스

- 통합 목차: [SUMMARY.md](SUMMARY.md)
- 문서 구조 맵 (언어/부분/기능): [structure/README.md](maintainers/structure-README.md)
- 문서 인벤토리/분류: [docs-inventory.md](maintainers/docs-inventory.md)
- i18n 문서 색인: [i18n/README.md](i18n/README.md)
- i18n 커버리지 맵: [i18n-coverage.md](maintainers/i18n-coverage.md)
- 프로젝트 트리아지 스냅샷: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)

## 다른 언어

- English: [README.md](README.md)
- 简体中文: [README.zh-CN.md](README.zh-CN.md)
- 日本語: [README.ja.md](README.ja.md)
- Русский: [README.ru.md](README.ru.md)
- Français: [README.fr.md](README.fr.md)
- Tiếng Việt: [i18n/vi/README.md](i18n/vi/README.md)
