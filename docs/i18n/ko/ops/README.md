# 운영 및 Deployment 문서

ZeroClaw를 지속적 또는 프로덕션 환경에서 운영하는 운영자를 위한 문서입니다.

## 핵심 운영

- Day-2 운영 런북: [./operations-runbook.md](./operations-runbook.md)
- 릴리스 런북: [../contributing/release-process.md](../contributing/release-process.md)
- 문제 해결 매트릭스: [./troubleshooting.md](./troubleshooting.md)
- 안전한 네트워크/gateway deployment: [./network-deployment.md](./network-deployment.md)
- Mattermost 설정 (채널별): [../setup-guides/mattermost-setup.md](../setup-guides/mattermost-setup.md)

## 일반적인 흐름

1. 런타임 검증 (`status`, `doctor`, `channel doctor`)
2. 설정 변경을 한 번에 하나씩 적용
3. 서비스/데몬 재시작
4. 채널 및 gateway 상태 확인
5. 동작이 퇴행하면 신속하게 롤백

## 관련 문서

- 설정 참조: [../reference/api/config-reference.md](../reference/api/config-reference.md)
- 보안 문서 모음: [../security/README.md](../security/README.md)
