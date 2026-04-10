# ZeroClaw 릴리스 프로세스

이 실행서는 메인테이너의 표준 릴리스 흐름을 정의합니다.

마지막 검증일: **2026년 2월 21일**.

## 릴리스 목표

- 릴리스를 예측 가능하고 반복 가능하게 유지합니다.
- 이미 `master`에 있는 코드에서만 게시합니다.
- 게시 전에 멀티 타겟 아티팩트를 검증합니다.
- 대량 PR에서도 릴리스 주기를 규칙적으로 유지합니다.

## 표준 주기

- 패치/마이너 릴리스: 주간 또는 격주.
- 긴급 보안 수정: 비정기.
- 매우 큰 커밋 배치가 축적되기를 기다리지 않습니다.

## 워크플로우 계약

릴리스 자동화의 위치:

- `.github/workflows/pub-release.yml`
- `.github/workflows/pub-homebrew-core.yml` (수동 Homebrew 포뮬라 PR, 봇 소유)
- `.github/workflows/pub-scoop.yml` (수동 Scoop 버킷 매니페스트 업데이트)
- `.github/workflows/pub-aur.yml` (수동 AUR PKGBUILD push)

모드:

- 태그 push `v*`: 게시 모드.
- 수동 dispatch: 검증 전용 또는 게시 모드.
- 주간 스케줄: 검증 전용 모드.

게시 모드 가드레일:

- 태그는 semver 유사 형식 `vX.Y.Z[-suffix]`와 일치해야 합니다.
- 태그가 이미 origin에 존재해야 합니다.
- 태그 커밋이 `origin/master`에서 도달 가능해야 합니다.
- GitHub Release 게시가 완료되기 전에 일치하는 GHCR 이미지 태그(`ghcr.io/<owner>/<repo>:<tag>`)가 사용 가능해야 합니다.
- 아티팩트는 게시 전에 검증됩니다.

## 메인테이너 절차

### 1) `master`에서 사전 점검

1. 최신 `master`에서 필수 검사가 녹색인지 확인합니다.
2. 높은 우선순위 인시던트나 알려진 회귀가 열려있지 않은지 확인합니다.
3. 최근 `master` 커밋에서 인스톨러 및 Docker 워크플로우가 정상인지 확인합니다.

### 2) 검증 빌드 실행 (게시 없이)

`Pub Release`를 수동으로 실행합니다:

- `publish_release`: `false`
- `release_ref`: `master`

예상 결과:

- 전체 타겟 매트릭스가 성공적으로 빌드됩니다.
- `verify-artifacts`가 모든 예상 아카이브의 존재를 확인합니다.
- GitHub Release가 게시되지 않습니다.

### 3) 릴리스 태그 생성

`origin/master`에 동기화된 깨끗한 로컬 체크아웃에서:

```bash
scripts/release/cut_release_tag.sh vX.Y.Z --push
```

이 스크립트는 다음을 강제합니다:

- 깨끗한 작업 트리
- `HEAD == origin/master`
- 중복되지 않는 태그
- semver 유사 태그 형식

### 4) 게시 실행 모니터링

태그 push 후 모니터링:

1. `Pub Release` 게시 모드
2. `Pub Docker Img` 게시 작업

예상 게시 출력:

- 릴리스 아카이브
- `SHA256SUMS`
- `CycloneDX` 및 `SPDX` SBOM
- cosign 서명/인증서
- GitHub Release 노트 + 자산

### 5) 릴리스 후 검증

1. GitHub Release 자산이 다운로드 가능한지 확인합니다.
2. 릴리스 버전(`vX.Y.Z`) 및 릴리스 커밋 SHA 태그(`sha-<12>`)에 대한 GHCR 태그를 확인합니다.
3. 릴리스 자산에 의존하는 설치 경로를 확인합니다 (예: 부트스트랩 바이너리 다운로드).

### 6) Homebrew Core 포뮬라 게시 (봇 소유)

`Pub Homebrew Core`를 수동으로 실행합니다:

- `release_tag`: `vX.Y.Z`
- `dry_run`: 먼저 `true`, 그 다음 `false`

비 dry-run에 필요한 저장소 설정:

- 시크릿: `HOMEBREW_CORE_BOT_TOKEN` (개인 메인테이너 계정이 아닌 전용 봇 계정의 토큰)
- 변수: `HOMEBREW_CORE_BOT_FORK_REPO` (예: `zeroclaw-release-bot/homebrew-core`)
- 선택적 변수: `HOMEBREW_CORE_BOT_EMAIL`

워크플로우 가드레일:

- 릴리스 태그가 `Cargo.toml` 버전과 일치해야 합니다
- 포뮬라 소스 URL과 SHA256이 태그된 tarball에서 업데이트됩니다
- 포뮬라 라이선스가 `Apache-2.0 OR MIT`로 정규화됩니다
- PR이 봇 포크에서 `Homebrew/homebrew-core:master`로 열립니다

### 7) Scoop 매니페스트 게시 (Windows)

`Pub Scoop Manifest`를 수동으로 실행합니다:

- `release_tag`: `vX.Y.Z`
- `dry_run`: 먼저 `true`, 그 다음 `false`

비 dry-run에 필요한 저장소 설정:

- 시크릿: `SCOOP_BUCKET_TOKEN` (버킷 저장소에 push 권한이 있는 PAT)
- 변수: `SCOOP_BUCKET_REPO` (예: `zeroclaw-labs/scoop-zeroclaw`)

워크플로우 가드레일:

- 릴리스 태그는 `vX.Y.Z` 형식이어야 합니다
- Windows 바이너리 SHA256이 `SHA256SUMS` 릴리스 자산에서 추출됩니다
- 매니페스트가 Scoop 버킷 저장소의 `bucket/zeroclaw.json`에 push됩니다

### 8) AUR 패키지 게시 (Arch Linux)

`Pub AUR Package`를 수동으로 실행합니다:

- `release_tag`: `vX.Y.Z`
- `dry_run`: 먼저 `true`, 그 다음 `false`

비 dry-run에 필요한 저장소 설정:

- 시크릿: `AUR_SSH_KEY` (AUR에 등록된 SSH 개인 키)

워크플로우 가드레일:

- 릴리스 태그는 `vX.Y.Z` 형식이어야 합니다
- 태그된 릴리스에서 소스 tarball SHA256이 계산됩니다
- PKGBUILD와 .SRCINFO가 AUR `zeroclaw` 패키지에 push됩니다

## 긴급 / 복구 경로

아티팩트가 검증된 후 태그 push 릴리스가 실패한 경우:

1. `master`에서 워크플로우 또는 패키징 문제를 수정합니다.
2. 게시 모드에서 수동 `Pub Release`를 재실행합니다:
   - `publish_release=true`
   - `release_tag=<기존 태그>`
   - `release_ref`는 게시 모드에서 `release_tag`에 자동으로 고정됩니다
3. 릴리스된 자산을 재검증합니다.

## 운영 참고사항

- 릴리스 변경을 작고 되돌리기 쉽게 유지합니다.
- 핸드오프가 명확하도록 버전당 하나의 릴리스 이슈/체크리스트를 선호합니다.
- 임시 기능 브랜치에서의 게시를 피합니다.
