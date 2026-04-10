# PR 규율

ZeroClaw에서의 pull request 품질, 저작자 표시, 개인정보 보호 및 핸드오프에 대한 규칙입니다.

## 개인정보 / 민감 데이터 (필수)

개인정보 보호와 중립성을 최선의 노력이 아닌 merge 게이트로 취급합니다.

- 코드, 문서, 테스트, 픽스처, 스냅샷, 로그, 예제 또는 커밋 메시지에 개인 또는 민감 데이터를 절대 커밋하지 않습니다.
- 금지된 데이터 (비한정적): 실명, 개인 이메일, 전화번호, 주소, 액세스 토큰, API 키, 자격 증명, ID, 비공개 URL.
- 실제 신원 데이터 대신 중립적이고 프로젝트 범위의 플레이스홀더를 사용합니다 (예: `user_a`, `test_user`, `project_bot`, `example.com`).
- 테스트 이름/메시지/픽스처는 비인격적이고 시스템 중심이어야 합니다; 1인칭이나 신원 특정 언어를 피합니다.
- 신원과 관련된 컨텍스트가 불가피한 경우, ZeroClaw 범위의 역할/라벨만 사용합니다 (예: `ZeroClawAgent`, `ZeroClawOperator`, `zeroclaw_user`).
- 권장하는 신원 안전 명명 팔레트:
    - 행위자 라벨: `ZeroClawAgent`, `ZeroClawOperator`, `ZeroClawMaintainer`, `zeroclaw_user`
    - 서비스/런타임 라벨: `zeroclaw_bot`, `zeroclaw_service`, `zeroclaw_runtime`, `zeroclaw_node`
    - 환경 라벨: `zeroclaw_project`, `zeroclaw_workspace`, `zeroclaw_channel`
- 외부 인시던트를 재현할 때는 커밋 전에 모든 페이로드를 수정하고 익명화합니다.
- push 전에 우발적인 민감 문자열 및 신원 유출을 확인하기 위해 `git diff --cached`를 구체적으로 검토합니다.

## 대체 PR 저작자 표시 (필수)

PR이 다른 기여자의 PR을 대체하고 실질적인 코드나 설계 결정을 계승하는 경우, 저작자를 명시적으로 보존합니다.

- 통합 커밋 메시지에 실질적으로 통합된 작업의 대체 기여자당 하나의 `Co-authored-by: Name <email>` 트레일러를 추가합니다.
- GitHub에서 인식하는 이메일을 사용합니다 (`<login@users.noreply.github.com>` 또는 기여자의 확인된 커밋 이메일).
- 트레일러를 커밋 메시지 끝의 빈 줄 다음에 별도의 줄에 유지합니다; 이스케이프된 `\\n` 텍스트로 인코딩하지 않습니다.
- PR 본문에 대체된 PR 링크를 나열하고 각각에서 무엇이 통합되었는지 간략히 설명합니다.
- 실제 코드/설계가 통합되지 않은 경우 (영감만 받은 경우), `Co-authored-by`를 사용하지 않습니다; 대신 PR 메모에서 크레딧을 표시합니다.

## 대체 PR 템플릿

### PR 제목/본문 템플릿

- 권장 제목 형식: `feat(<scope>): unify and supersede #<pr_a>, #<pr_b> [and #<pr_n>]`
- PR 본문에 포함:

```md
## Supersedes
- #<pr_a> by @<author_a>
- #<pr_b> by @<author_b>

## Integrated Scope
- From #<pr_a>: <실질적으로 통합된 내용>
- From #<pr_b>: <실질적으로 통합된 내용>

## Attribution
- 실질적으로 통합된 기여자에 대한 Co-authored-by 트레일러 추가: Yes/No
- No인 경우, 이유 설명

## Non-goals
- <계승하지 않은 것을 명시적으로 나열>

## Risk and Rollback
- Risk: <요약>
- Rollback: <revert 커밋/PR 전략>
```

### 커밋 메시지 템플릿

```text
feat(<scope>): unify and supersede #<pr_a>, #<pr_b> [and #<pr_n>]

<통합 결과의 한 단락 요약>

Supersedes:
- #<pr_a> by @<author_a>
- #<pr_b> by @<author_b>

Integrated scope:
- <subsystem_or_feature_a>: from #<pr_x>
- <subsystem_or_feature_b>: from #<pr_y>

Co-authored-by: <Name A> <login_a@users.noreply.github.com>
Co-authored-by: <Name B> <login_b@users.noreply.github.com>
```

## 핸드오프 템플릿 (에이전트 -> 에이전트 / 메인테이너)

작업을 넘길 때 다음을 포함합니다:

1. 변경된 것
2. 변경되지 않은 것
3. 검증 실행 및 결과
4. 남은 리스크 / 미지의 사항
5. 다음 권장 조치
