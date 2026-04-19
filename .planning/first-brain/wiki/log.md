---
type: log
title: 운영 로그
updated: 2026-04-18
---

# 운영 로그

> append-only. 각 이벤트는 `## [YYYY-MM-DD] kind | title` 형식 헤더로 시작.
> kind 종류: `ingest`, `query`, `lint`, `reflect`, `schema`.
> 빠른 조회: 파일 끝에서 위로 읽는다. `grep "^## \[" log.md | tail -N`

---

## [2026-04-18] schema | First Brain 스키마 초기화
- `.planning/first-brain/` 트리 생성.
- `README.md`, `AGENTS.md`, `wiki/index.md`, `wiki/log.md`, `wiki/overview.md` 초안 작성.
- 카테고리별 `_template.md` 배치: people, experiences, work, thoughts, themes, sources.
- `wiki/people/me.md` 초안 배치 — 사용자와의 첫 대화에서 채워질 예정.
