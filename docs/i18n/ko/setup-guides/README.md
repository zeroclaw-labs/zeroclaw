# 시작 가이드 문서

처음 설정하고 빠르게 살펴보기 위한 문서입니다.

## 시작 경로

1. 주요 개요 및 빠른 시작: [../../README.md](../../README.md)
2. 원클릭 설정 및 듀얼 부트스트랩 모드: [one-click-bootstrap.md](one-click-bootstrap.md)
3. macOS에서 업데이트 또는 제거: [macos-update-uninstall.md](macos-update-uninstall.md)
4. 작업별 명령어 찾기: [../reference/cli/commands-reference.md](../reference/cli/commands-reference.md)
5. MCP 서버 등록: [mcp-setup.md](mcp-setup.md)

## 경로를 선택하세요

| 시나리오 | 명령어 |
|----------|---------|
| API 키가 있고, 가장 빠른 설정을 원합니다 | `zeroclaw onboard --api-key sk-... --provider openrouter` |
| 안내된 프롬프트를 원합니다 | `zeroclaw onboard` |
| config가 이미 있고, 채널만 수정합니다 | `zeroclaw onboard --channels-only` |
| config가 이미 있고, 의도적으로 전체 덮어쓰기를 원합니다 | `zeroclaw onboard --force` |
| Subscription auth를 사용합니다 | [Subscription Auth](../../README.md#subscription-auth-openai-codex--claude-code) 참조 |

## 온보딩 및 검증

- 빠른 온보딩: `zeroclaw onboard --api-key "sk-..." --provider openrouter`
- 안내 온보딩: `zeroclaw onboard`
- 기존 config 보호: 재실행 시 명시적 확인이 필요합니다 (비대화형 흐름에서는 `--force` 사용)
- Ollama cloud 모델 (`:cloud`)은 원격 `api_url`과 API 키가 필요합니다 (예: `api_url = "https://ollama.com"`).
- 환경 검증: `zeroclaw status` + `zeroclaw doctor`

## 다음 단계

- 런타임 운영: [../ops/README.md](../ops/README.md)
- 참조 카탈로그: [../reference/README.md](../reference/README.md)
- macOS 라이프사이클 작업: [macos-update-uninstall.md](macos-update-uninstall.md)
