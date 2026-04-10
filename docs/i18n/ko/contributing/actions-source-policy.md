# Actions 소스 정책

이 문서는 이 저장소의 현재 GitHub Actions 소스 제어 정책을 정의합니다.

## 현재 정책

- 저장소 Actions 권한: 활성화
- 허용 actions 모드: 선택적

선택적 허용 목록 (Quality Gate, Release Beta, Release Stable 워크플로우에서 현재 사용 중인 모든 actions):

| Action | 사용 위치 | 목적 |
|--------|---------|---------|
| `actions/checkout@v4` | 모든 워크플로우 | 저장소 체크아웃 |
| `actions/upload-artifact@v4` | release, promote-release | 빌드 아티팩트 업로드 |
| `actions/download-artifact@v4` | release, promote-release | 패키징을 위한 빌드 아티팩트 다운로드 |
| `dtolnay/rust-toolchain@stable` | 모든 워크플로우 | Rust 툴체인 설치 (1.92.0) |
| `Swatinem/rust-cache@v2` | 모든 워크플로우 | Cargo 빌드/의존성 캐싱 |
| `softprops/action-gh-release@v2` | release, promote-release | GitHub Release 생성 |
| `docker/setup-buildx-action@v3` | release, promote-release | Docker Buildx 설정 |
| `docker/login-action@v3` | release, promote-release | GHCR 인증 |
| `docker/build-push-action@v6` | release, promote-release | 멀티 플랫폼 Docker 이미지 빌드 및 push |
| `actions/labeler@v5` | pr-path-labeler | `labeler.yml`에서 경로/범위 라벨 적용 |

동등한 허용 목록 패턴:

- `actions/*`
- `dtolnay/rust-toolchain@*`
- `Swatinem/rust-cache@*`
- `softprops/action-gh-release@*`
- `docker/*`

## 워크플로우

| 워크플로우 | 파일 | 트리거 |
|----------|------|---------|
| Quality Gate | `.github/workflows/checks-on-pr.yml` | `master`로의 Pull request |
| Release Beta | `.github/workflows/release-beta-on-push.yml` | `master`로 Push |
| Release Stable | `.github/workflows/release-stable-manual.yml` | 수동 `workflow_dispatch` |
| PR Path Labeler | `.github/workflows/pr-path-labeler.yml` | `pull_request_target` (opened, synchronize, reopened) |

## 변경 관리

각 정책 변경을 다음과 함께 기록합니다:

- 변경 날짜/시간 (UTC)
- 수행자
- 이유
- 허용 목록 변경사항 (추가/제거된 패턴)
- 롤백 메모

현재 유효 정책을 내보내려면 다음 명령을 사용합니다:

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

## 가드레일

- `uses:` action 소스를 추가하거나 변경하는 모든 PR은 허용 목록 영향 메모를 포함해야 합니다.
- 새로운 서드파티 action은 허용 목록에 추가하기 전에 메인테이너의 명시적 리뷰가 필요합니다.
- 검증된 누락 action에 대해서만 허용 목록을 확장합니다; 광범위한 와일드카드 예외를 피합니다.

## 변경 로그

- 2026-03-23: `actions/labeler@v5`를 사용하는 PR Path Labeler (`pr-path-labeler.yml`) 추가. 기존 `actions/*` 패턴으로 커버되므로 허용 목록 변경 불필요.
- 2026-03-10: 워크플로우 이름 변경 — CI -> Quality Gate (`checks-on-pr.yml`), Beta Release -> Release Beta (`release-beta-on-push.yml`), Promote Release -> Release Stable (`release-stable-manual.yml`). Quality Gate에 `lint` 및 `security` 작업 추가. Cross-Platform Build (`cross-platform-build-manual.yml`) 추가.
- 2026-03-05: 완전한 워크플로우 전면 개편 — 22개 워크플로우를 3개(CI, Beta Release, Promote Release)로 교체
    - 더 이상 사용하지 않는 패턴 제거: `DavidAnson/markdownlint-cli2-action@*`, `lycheeverse/lychee-action@*`, `EmbarkStudios/cargo-deny-action@*`, `rustsec/audit-check@*`, `rhysd/actionlint@*`, `sigstore/cosign-installer@*`, `Checkmarx/vorpal-reviewdog-github-action@*`, `useblacksmith/*`
    - 추가: `Swatinem/rust-cache@*` (`useblacksmith/*` rust-cache 포크 대체)
    - 유지: `actions/*`, `dtolnay/rust-toolchain@*`, `softprops/action-gh-release@*`, `docker/*`
- 2026-03-05: CI 빌드 최적화 — mold 링커, cargo-nextest, CARGO_INCREMENTAL=0 추가
    - 취약한 GHA 캐시 백엔드로 인한 빌드 실패로 sccache 제거

## 롤백

긴급 차단 해제 경로:

1. 일시적으로 Actions 정책을 `all`로 되돌립니다.
2. 누락된 항목을 식별한 후 선택적 허용 목록을 복원합니다.
3. 인시던트와 최종 허용 목록 변경사항을 기록합니다.
