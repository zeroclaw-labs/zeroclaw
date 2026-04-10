# 문서 시스템 계약

문서를 merge 후 산출물이 아닌 일급 제품 표면으로 취급합니다.

## 정식 진입점

- 루트 README: `README.md`, `README.zh-CN.md`, `README.ja.md`, `README.ru.md`, `README.fr.md`, `README.vi.md`
- 문서 허브: `docs/README.md`, `docs/README.zh-CN.md`, `docs/README.ja.md`, `docs/README.ru.md`, `docs/README.fr.md`, `docs/README.vi.md`
- 통합 목차: `docs/SUMMARY.md`

## 지원 로케일

`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`

## 컬렉션 인덱스

- `docs/setup-guides/README.md`
- `docs/reference/README.md`
- `docs/ops/README.md`
- `docs/security/README.md`
- `docs/hardware/README.md`
- `docs/contributing/README.md`
- `docs/maintainers/README.md`

## 거버넌스 규칙

- README/허브 최상위 탐색과 빠른 경로를 직관적이고 중복되지 않게 유지합니다.
- 탐색 아키텍처를 변경할 때 지원되는 모든 로케일에서 진입점 동등성을 유지합니다.
- 변경이 문서 IA, 런타임 계약 참조 또는 공유 문서의 사용자 대면 문구에 영향을 미치는 경우, 동일 PR에서 지원 로케일에 대한 i18n 후속 작업을 수행합니다:
  - 로케일 탐색 링크를 업데이트합니다 (`README*`, `docs/README*`, `docs/SUMMARY.md`).
  - 동등한 버전이 존재하는 곳에서 현지화된 런타임 계약 문서를 업데이트합니다.
  - 베트남어의 경우, `docs/vi/**`를 정식으로 취급합니다.
- 제안/로드맵 문서에 명시적으로 라벨을 붙입니다; 제안 텍스트를 런타임 계약 문서에 혼합하지 않습니다.
- 프로젝트 스냅샷은 날짜를 명시하고 더 새로운 날짜의 것으로 대체되면 변경 불가로 유지합니다.
