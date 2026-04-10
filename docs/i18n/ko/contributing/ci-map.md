# CI 워크플로우 맵

이 문서는 각 GitHub 워크플로우가 무엇을 하는지, 언제 실행되는지, merge를 차단해야 하는지 설명합니다.

이벤트별 전달 동작(PR, merge, push, 릴리스)에 대해서는 [`.github/workflows/master-branch-flow.md`](../../.github/workflows/master-branch-flow.md)를 참조합니다.

## Merge 차단 vs 선택적

merge 차단 검사는 작고 결정적이어야 합니다. 선택적 검사는 자동화와 유지보수에 유용하지만, 일반 개발을 차단해서는 안 됩니다.

### Merge 차단

- `.github/workflows/ci-run.yml` (`CI`)
    - 목적: Rust 검증 (`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D clippy::correctness`, 변경된 Rust 라인에 대한 엄격한 delta lint 게이트, `test`, 릴리스 빌드 스모크) + 문서 변경 시 문서 품질 검사 (`markdownlint`는 변경된 라인의 이슈만 차단; 링크 검사는 변경된 라인에 추가된 링크만 스캔)
    - 추가 동작: Rust에 영향을 미치는 PR 및 push의 경우, `CI Required Gate`는 `lint` + `test` + `build`를 요구합니다 (PR 전용 빌드 우회 없음)
    - 추가 동작: `.github/workflows/**`를 변경하는 PR은 `WORKFLOW_OWNER_LOGINS` (저장소 변수 폴백: `theonlyhennygod,JordanTheJet,SimianAstronaut7`)에 있는 로그인의 승인 리뷰가 최소 1건 필요합니다
    - 추가 동작: lint 게이트가 `test`/`build` 전에 실행됩니다; PR에서 lint/문서 게이트가 실패하면, CI가 실패한 게이트 이름과 로컬 수정 명령이 포함된 실행 가능한 피드백 코멘트를 게시합니다
    - Merge 게이트: `CI Required Gate`
- `.github/workflows/workflow-sanity.yml` (`Workflow Sanity`)
    - 목적: GitHub 워크플로우 파일 lint (`actionlint`, 탭 검사)
    - 워크플로우 변경 PR에 권장
- `.github/workflows/pr-intake-checks.yml` (`PR Intake Checks`)
    - 목적: 안전한 사전 CI PR 검사 (템플릿 완성도, 추가된 라인의 탭/후행 공백/충돌 마커) 및 즉시 고정 피드백 코멘트

### 비차단이지만 중요

- `.github/workflows/pub-docker-img.yml` (`Docker`)
    - 목적: `master` PR에 대한 PR Docker 스모크 검사 및 태그 push(`v*`)에서만 이미지 게시
- `.github/workflows/sec-audit.yml` (`Security Audit`)
    - 목적: 의존성 권고 (`rustsec/audit-check`, 고정 SHA) 및 정책/라이선스 검사 (`cargo deny`)
- `.github/workflows/sec-codeql.yml` (`CodeQL Analysis`)
    - 목적: 보안 발견을 위한 예약/수동 정적 분석
- `.github/workflows/sec-vorpal-reviewdog.yml` (`Sec Vorpal Reviewdog`)
    - 목적: 지원되는 비 Rust 파일(`.py`, `.js`, `.jsx`, `.ts`, `.tsx`)에 대한 수동 보안 코딩 피드백 스캔 (reviewdog 어노테이션 사용)
    - 노이즈 제어: 기본적으로 일반 테스트/픽스처 경로 및 테스트 파일 패턴을 제외합니다 (`include_tests=false`)
- `.github/workflows/pub-release.yml` (`Release`)
    - 목적: 검증 모드(수동/예약)에서 릴리스 아티팩트를 빌드하고 태그 push 또는 수동 게시 모드에서 GitHub 릴리스를 게시
- `.github/workflows/pub-homebrew-core.yml` (`Pub Homebrew Core`)
    - 목적: 태그된 릴리스에 대한 수동, 봇 소유 Homebrew core 포뮬라 범프 PR 흐름
    - 가드레일: 릴리스 태그가 `Cargo.toml` 버전과 일치해야 합니다
- `.github/workflows/pub-scoop.yml` (`Pub Scoop Manifest`)
    - 목적: Windows용 Scoop 버킷 매니페스트 업데이트; 안정 릴리스에 의해 자동 호출, 수동 dispatch도 가능
    - 가드레일: 릴리스 태그는 `vX.Y.Z` 형식이어야 합니다; Windows 바이너리 해시는 `SHA256SUMS`에서 추출
- `.github/workflows/pub-aur.yml` (`Pub AUR Package`)
    - 목적: Arch Linux용 AUR PKGBUILD push; 안정 릴리스에 의해 자동 호출, 수동 dispatch도 가능
    - 가드레일: 릴리스 태그는 `vX.Y.Z` 형식이어야 합니다; 소스 tarball SHA256은 게시 시 계산
- `.github/workflows/pr-label-policy-check.yml` (`Label Policy Sanity`)
    - 목적: `.github/label-policy.json`의 공유 기여자 등급 정책을 검증하고 라벨 워크플로우가 해당 정책을 사용하는지 확인
- `.github/workflows/test-rust-build.yml` (`Rust Reusable Job`)
    - 목적: 워크플로우 호출 소비자를 위한 재사용 가능한 Rust 설정/캐시 + 명령 러너

### 선택적 저장소 자동화

- `.github/workflows/pr-labeler.yml` (`PR Labeler`)
    - 목적: 범위/경로 라벨 + 크기/리스크 라벨 + 세분화된 모듈 라벨 (`<module>: <component>`)
    - 추가 동작: 라벨 설명이 각 자동 판단 규칙을 설명하는 호버 툴팁으로 자동 관리됩니다
    - 추가 동작: provider 관련 키워드가 provider/config/onboard/integration 변경에서 `provider:*` 라벨로 승격됩니다 (예: `provider:kimi`, `provider:deepseek`)
    - 추가 동작: 계층적 중복 제거로 가장 구체적인 범위 라벨만 유지합니다 (예: `tool:composio`가 `tool:core`와 `tool`을 억제)
    - 추가 동작: 모듈 네임스페이스가 압축됩니다 — 하나의 구체적 모듈은 `prefix:component`를 유지하고, 여러 구체적 모듈은 `prefix`로 축소
    - 추가 동작: merge된 PR 수에 따라 PR에 기여자 등급을 적용합니다 (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50)
    - 추가 동작: 최종 라벨 세트는 우선순위 정렬됩니다 (`risk:*` 먼저, 그 다음 `size:*`, 기여자 등급, 모듈/경로 라벨)
    - 추가 동작: 관리 라벨 색상은 많은 라벨이 있을 때 부드러운 좌-우 그라데이션을 생성하도록 표시 순서를 따릅니다
    - 수동 거버넌스: `workflow_dispatch`를 `mode=audit|repair`로 지원하여 전체 저장소에서 관리 라벨 메타데이터 드리프트를 검사/수정
    - 추가 동작: 수동 PR 라벨 편집(`labeled`/`unlabeled` 이벤트)에서 리스크 + 크기 라벨이 자동 교정됩니다; 메인테이너가 의도적으로 자동 리스크 선택을 오버라이드할 때 `risk: manual`을 적용
    - 고위험 휴리스틱 경로: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`
    - 가드레일: 메인테이너가 `risk: manual`을 적용하여 자동 리스크 재계산을 동결할 수 있습니다
- `.github/workflows/pr-auto-response.yml` (`PR Auto Responder`)
    - 목적: 첫 기여자 온보딩 + 라벨 기반 응답 라우팅 (`r:support`, `r:needs-repro` 등)
    - 추가 동작: merge된 PR 수에 따라 이슈에 기여자 등급을 적용합니다 (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50), PR 등급 임계값과 정확히 일치
    - 추가 동작: 기여자 등급 라벨은 자동화 관리로 취급됩니다 (PR/이슈에서의 수동 추가/제거가 자동 교정됨)
    - 가드레일: 라벨 기반 닫기 경로는 이슈 전용이며, PR은 라우트 라벨에 의해 자동으로 닫히지 않습니다
- `.github/workflows/pr-check-stale.yml` (`Stale`)
    - 목적: stale 이슈/PR 생명주기 자동화
- `.github/dependabot.yml` (`Dependabot`)
    - 목적: 그룹화되고 속도 제한된 의존성 업데이트 PR (Cargo + GitHub Actions)
- `.github/workflows/pr-check-status.yml` (`PR Hygiene`)
    - 목적: 큐 기아 전에 stale이지만 활성인 PR에 rebase/필수 검사 재실행을 알림

## 트리거 맵

- `CI`: `master`로 push, `master`로의 PR
- `Docker`: 게시용 태그 push(`v*`), 스모크 빌드용 `master`로의 매칭 PR, 스모크 전용 수동 dispatch
- `Release`: 태그 push(`v*`), 주간 스케줄(검증 전용), 수동 dispatch(검증 또는 게시)
- `Pub Homebrew Core`: 수동 dispatch 전용
- `Pub Scoop Manifest`: 안정 릴리스에 의해 자동 호출, 수동 dispatch도 가능
- `Pub AUR Package`: 안정 릴리스에 의해 자동 호출, 수동 dispatch도 가능
- `Security Audit`: `master`로 push, `master`로의 PR, 주간 스케줄
- `Sec Vorpal Reviewdog`: 수동 dispatch 전용
- `Workflow Sanity`: `.github/workflows/**`, `.github/*.yml` 또는 `.github/*.yaml` 변경 시 PR/push
- `Dependabot`: 모든 업데이트 PR은 `master`를 대상으로 함
- `PR Intake Checks`: opened/reopened/synchronize/edited/ready_for_review에서 `pull_request_target`
- `Label Policy Sanity`: `.github/label-policy.json`, `.github/workflows/pr-labeler.yml` 또는 `.github/workflows/pr-auto-response.yml` 변경 시 PR/push
- `PR Labeler`: `pull_request_target` 생명주기 이벤트
- `PR Auto Responder`: 이슈 opened/labeled, `pull_request_target` opened/labeled
- `Stale PR Check`: 일일 스케줄, 수동 dispatch
- `PR Hygiene`: 12시간마다 스케줄, 수동 dispatch

## 빠른 분류 가이드

1. `CI Required Gate` 실패: `.github/workflows/ci-run.yml`부터 시작합니다.
2. PR에서 Docker 실패: `.github/workflows/pub-docker-img.yml` `pr-smoke` 작업을 점검합니다.
3. 릴리스 실패 (태그/수동/예약): `.github/workflows/pub-release.yml` 및 `prepare` 작업 출력을 점검합니다.
4. Homebrew 포뮬라 게시 실패: `.github/workflows/pub-homebrew-core.yml` 요약 출력과 봇 토큰/포크 변수를 점검합니다.
5. Scoop 매니페스트 게시 실패: `.github/workflows/pub-scoop.yml` 요약 출력과 `SCOOP_BUCKET_REPO`/`SCOOP_BUCKET_TOKEN` 설정을 점검합니다.
6. AUR 패키지 게시 실패: `.github/workflows/pub-aur.yml` 요약 출력과 `AUR_SSH_KEY` 시크릿을 점검합니다.
7. 보안 실패: `.github/workflows/sec-audit.yml`과 `deny.toml`을 점검합니다.
8. 워크플로우 문법/lint 실패: `.github/workflows/workflow-sanity.yml`을 점검합니다.
9. PR 접수 실패: `.github/workflows/pr-intake-checks.yml` 고정 코멘트와 실행 로그를 점검합니다.
10. 라벨 정책 동등성 실패: `.github/workflows/pr-label-policy-check.yml`을 점검합니다.
11. CI에서 문서 실패: `.github/workflows/ci-run.yml`의 `docs-quality` 작업 로그를 점검합니다.
12. CI에서 엄격한 delta lint 실패: `lint-strict-delta` 작업 로그를 점검하고 `BASE_SHA` diff 범위와 비교합니다.

## 유지보수 규칙

- merge 차단 검사를 결정적이고 재현 가능하게 유지합니다 (해당하는 경우 `--locked`).
- 검증 후 게시 릴리스 주기와 태그 규율에 대해서는 [`docs/contributing/release-process.md`](./release-process.md)를 따릅니다.
- merge 차단 Rust 품질 정책을 `.github/workflows/ci-run.yml`, `dev/ci.sh`, `.githooks/pre-push` (`./scripts/ci/rust_quality_gate.sh` + `./scripts/ci/rust_strict_delta_gate.sh`) 전체에서 정렬 상태로 유지합니다.
- `./scripts/ci/rust_strict_delta_gate.sh` (또는 `./dev/ci.sh lint-delta`)를 변경된 Rust 라인의 점진적 엄격 merge 게이트로 사용합니다.
- `./scripts/ci/rust_quality_gate.sh --strict` (예: `./dev/ci.sh lint-strict`)를 통해 정기적으로 전체 엄격 lint 감사를 실행하고 집중 PR에서 정리를 추적합니다.
- `./scripts/ci/docs_quality_gate.sh`를 통해 문서 마크다운 게이팅을 점진적으로 유지합니다 (변경된 라인 이슈를 차단하고, 기준 이슈는 별도 보고).
- `./scripts/ci/collect_changed_links.py` + lychee를 통해 문서 링크 게이팅을 점진적으로 유지합니다 (변경된 라인에 추가된 링크만 검사).
- 명시적 워크플로우 권한을 선호합니다 (최소 권한).
- Actions 소스 정책을 승인된 허용 목록 패턴으로 제한합니다 ([`docs/contributing/actions-source-policy.md`](./actions-source-policy.md) 참조).
- 비용이 큰 워크플로우에는 실용적인 경우 경로 필터를 사용합니다.
- 문서 품질 검사를 저노이즈로 유지합니다 (점진적 마크다운 + 점진적 추가 링크 검사).
- 의존성 업데이트 볼륨을 제어합니다 (그룹화 + PR 제한).
- 온보딩/커뮤니티 자동화와 merge 게이팅 로직을 혼합하지 않습니다.
- 테스트 레벨: `cargo test --test component`, `cargo test --test integration`, `cargo test --test system`.
- 라이브 테스트 (수동만): `cargo test --test live -- --ignored`.

## 자동화 부작용 제어

- 컨텍스트가 미묘한 경우 수동으로 오버라이드할 수 있는(`risk: manual`) 결정적 자동화를 선호합니다.
- 자동 응답 코멘트를 중복 제거하여 분류 노이즈를 방지합니다.
- 자동 닫기 동작을 이슈로 제한합니다; 메인테이너가 PR 닫기/merge 결정을 소유합니다.
- 자동화가 잘못된 경우, 먼저 라벨을 수정한 다음 명시적 근거와 함께 리뷰를 계속합니다.
- 심층 리뷰 전에 중복 또는 휴면 PR을 정리하기 위해 `superseded` / `stale-candidate` 라벨을 사용합니다.
