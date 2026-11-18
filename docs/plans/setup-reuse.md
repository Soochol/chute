# `chute init --setup` 직접 머지 경로의 PR 재사용·브랜치 덮어쓰기 플랜

> 작성일 2026-11-18. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

`chute init --setup`(앞 플랜 `docs/plans/chute-setup.md`, 4~5단계는 PR #97)은 `chute/setup` 브랜치를 push하고 PR을 만든 뒤 chute가 기본 브랜치에 직접 머지한다. PR #97의 독립 리뷰가 네 빈틈을 찾았고 재현 시험이 모두 실패했다(보고서 `/home/dev/.claude/orch/reports/setup-merge/setup-merge.md` "미해결 사항, 위험" 1~4, 재현 패치 `/home/dev/.claude/orch/reports/setup-merge/interrogate-opus-repro.patch`). 사용자가 고치기로 승인했다.

1. 열린 PR이 없는데 원격에 `chute/setup`이 이미 있으면 확인 없이 덮어쓴다. `publish`가 방금 읽은 원격 끝을 lease 기대값으로 쓰므로 lease는 경주만 막는다.
2. 재사용한 열린 PR의 HEAD가 같은 내용에 다른 부모(검증하지 않은 이력)를 달고 있어도 머지한다. 트리는 검증한 것과 같지만 그 이력이 기본 브랜치에 들어간다.
3. 저장소에 `* text=auto`가 있고 작업 폴더 `chute.toml`이 CRLF이면 머지되지 않고 재실행마다 새 커밋을 push한다.
4. 재사용한 PR HEAD가 실행 비트만 다르면 ⑤에서 영영 거절되고 PR HEAD를 다시 만들지 않는다.

PR #97 HEAD `7b6cb19`에서 확인한 원인:

- 재사용 판정(`merge.rs`의 `has_setup_content`)은 PR HEAD의 `chute.toml`과 지침 파일 바이트가 `ready.files`와 같은지만 본다. 부모와 파일 모드와 다른 파일은 보지 않는다(2, 4). 내용이 같다고 재사용한 HEAD가 다른 이유로 ⑤에서 거절되면 다음 실행도 같은 HEAD를 재사용한다(4).
- `ready.files`는 작업 폴더의 원본 바이트이고, setup 커밋(`Slot::commit_files`)은 `git add`를 거쳐 `.gitattributes`대로 정규화된다. 조건 ④(`conditions`)와 재사용 판정이 커밋의 blob을 원본 바이트와 비교하므로 CRLF 초안은 방금 만든 커밋도 ④에서 거절되고, 재실행은 재사용하지 못해 새 커밋을 push한다(3). 머지 뒤 작업 폴더 정리(`finish`)도 원본 바이트를 비교하므로 CRLF 초안은 지우지 못한다.
- 열린 PR이 없으면 `publish`가 `remote_branch_tip`을 읽어 바로 lease로 쓴다. 확인 단계(`setup.rs`의 `drive`)는 원격 브랜치를 보지 않는다(1).

## 2. 확정된 결정

요청서(사용자 승인)에서 온 것이다.

- 작은 작업 하나, PR 하나로 고친다.
- 1: 덮어쓰기 전에 확인 단계(터미널의 `y` 또는 `--yes`)에서 "원격에 `chute/setup` 브랜치가 이미 있고 이를 덮어쓴다(그 HEAD SHA)"가 보여야 한다. 사용자 커밋이 몰래 사라지지 않아야 한다. 멈출지 알릴지는 플래너가 정한다(3절).
- 2~4: 재사용 조건을 "HEAD의 트리가 `ready.tree`이고 부모가 `ready.base` 하나"로 바꾼다. 맞지 않으면 새 setup 커밋을 만들어 lease로 push한다.
- 기본 브랜치에는 검증한 트리만 들어간다(⑤ 트리 비교)는 조건은 그대로 지킨다.
- 재현 시험 넷은 기대 동작(덮어쓰기 알림/거절, 이력 차단, CRLF에서도 끝남, 실행 비트 PR은 새로 만듦)으로 바뀌어 통과한다. 기존 setup 시험도 통과한다.

## 3. 스스로 정한 결정

| 결정 | 이유 |
|---|---|
| **재사용할 수 있는 커밋**: 트리가 `ready.tree`이고 부모가 정확히 `ready.base` 하나인 커밋. 열린 PR의 HEAD와 열린 PR 없이 남은 원격 `chute/setup` 끝에 같은 판정을 쓴다. 내용 바이트 비교(`has_setup_content`)는 없앤다 | 트리가 같으면 파일 내용과 모드와 다른 파일 유무가 모두 같고, 부모가 기준 커밋 하나면 따라 들어오는 이력이 없다. 이 커밋을 머지하면 ⑤가 늘 통과한다. 2~4가 한 조건으로 풀린다 |
| 열린 PR의 HEAD가 재사용할 수 없으면 지금처럼 새 setup 커밋을 만들어 `--force-with-lease=refs/heads/chute/setup:<그 HEAD>`로 push하고 같은 PR을 쓴다. `--yes`에서도 그렇다. 출력에 덮어쓴 HEAD SHA를 적는다. 터미널 확인 문구도 "내용이 다르면 새 setup 커밋으로 덮어쓴다"와 그 PR의 HEAD SHA를 적는다 | `chute/setup`에서 기본 브랜치로 가는 열린 PR은 setup PR이고 덮어쓰기는 앞 플랜의 결정이다. 초안을 고친 재실행(`a_rerun_after_the_draft_was_edited_pushes_a_new_commit_by_lease_to_the_same_pr`)과 4의 기대 동작("새로 만듦")이 `--yes`에서 이 덮어쓰기를 요구한다. force push는 PR 타임라인에 남는다 |
| **열린 PR이 없는데 원격 `chute/setup`이 있을 때**: 그 끝이 재사용할 수 있으면 push하지 않고 그 커밋으로 PR을 만든다. 재사용할 수 없으면 터미널에서는 확인 문구에 "원격에 chute/setup이 이미 있고 열린 PR이 없다. 그 HEAD `<SHA>`를 새 setup 커밋으로 덮어쓴다"를 적고 `y`를 받은 뒤에만 그 SHA를 lease로 덮어쓴다. `--yes`이면 덮어쓰지 않고 멈춘다(종료 코드 1). 멈춤 문구는 SHA와 두 길(터미널에서 확인, 필요 없으면 `git push <원격> --delete chute/setup`으로 지운 뒤 다시 실행)을 적는다 | 이 브랜치는 다른 base로 가는 PR의 head이거나 사람이 쓰던 브랜치일 수 있다. `--yes`는 비터미널 실행이 미리 보인 diff에 대한 동의인데, 그 실행은 원격 브랜치를 보지 않고 멈추므로 덮어쓰기는 동의 범위 밖이다. 재사용 가능한 끝(앞 실행이 push 뒤 `gh pr create` 전에 끊긴 경우)은 그대로 이어 가 재실행이 멱등이다 |
| 터미널에서 본 SHA가 lease 기대값이다. 확인과 push 사이에 원격 끝이 바뀌었으면(또는 확인 때 없던 브랜치가 생겼으면) 덮어쓰지 않고 멈춘다. 동의는 diff와 덮어쓸 SHA의 쌍에 유효하다. 재시도에서 둘 중 하나가 바뀌면 다시 묻는다 | 확인한 것만 덮어쓴다. 지금 `drive`의 "diff가 바뀌면 다시 묻는다"와 같은 규칙이다 |
| 원격 끝은 확인 전에 `git ls-remote`(`Ext`를 거친 읽기)로 SHA만 읽는다. 재사용 판정에 필요한 객체 가져오기와 판정은 확인 뒤 슬롯 잠금 안(`land`)에서 한다 | 사람의 답을 기다리는 동안 preflight 슬롯 잠금을 쥐지 않는다. 사용자 작업 폴더의 ref를 바꾸지 않는다 |
| 조건 ③ 뒤에 "HEAD의 트리가 `ready.tree`이고 부모가 기준 커밋 하나"를 머지 직전 조건으로 더한다. 기존 ②~④ 검사 뒤에 둬 기존 거절 문구가 먼저 나오게 한다. ④의 내용 비교는 원본 바이트가 아니라 검증한 트리의 blob(`<ready.tree>:<경로>`)과 비교한다 | `publish`가 바뀌어도 머지 직전 한 함수가 이력을 막는다. ④를 정규화된 blob과 비교해야 CRLF 초안이 끝난다. 앞 플랜의 "조건은 한 함수에서" 결정을 지킨다 |
| `finish`(머지 뒤 작업 폴더 `chute.toml` 정리)는 작업 폴더 파일을 저장소의 attributes대로 정규화한 blob(`git hash-object --path=chute.toml`과 같은 계산)이 머지된 blob과 같을 때 지운다 | CRLF 초안이 남으면 사용자의 다음 `git pull`이 "untracked working tree files would be overwritten"으로 멈춘다. "CRLF에서도 끝남"에 이것까지 넣는다 |
| 기존 시험 `a_pr_head_that_changes_files_besides_the_config_and_the_instructions_is_refused`는 둘로 나눈다. 끝까지 도는 시험은 "다른 파일을 더한 PR HEAD는 새로 만들어지고 그 파일은 기본 브랜치에 없다"로 바꾸고, ②(밖의 파일) 거절은 `conditions`를 직접 부르는 시험으로 남긴다 | 새 재사용 조건에서는 그 HEAD가 `conditions`에 닿기 전에 새 커밋으로 바뀐다. 조건 ②의 시험은 `an_instructions_change_that_is_not_the_one_line_is_refused_by_the_conditions`처럼 직접 부르면 그대로 고정된다 |
| 재현 패치의 `zz_` 시험은 그대로 넣지 않고 기대 동작을 이름에 담은 시험으로 다시 쓴다 | 재현 패치는 임시 이름이고 기대 동작과 반대 단언이 섞여 있다 |
| README "처음 쓰기"의 재실행 문장을 새 규칙으로 고친다 | 사용자에게 보이는 동작이 바뀐다 |
| 작업 하나, `profile` `implementer`, `mode` `poteto` | main에 무엇이 들어가는지 정하는 코드(직접 머지의 조건과 push)다 |

## 4. 작업

| 작업 | 방식 | 이유 |
|---|---|---|
| `setup-reuse` | `poteto`, `implementer` | 기본 브랜치 직접 머지의 재사용 조건, 머지 조건, 원격 브랜치 덮어쓰기를 고친다 |

작업이 하나라 병렬·순차 관계는 없다. 이 작업은 PR #97(`orch/setup-merge/setup-merge`)이 main에 들어간 뒤 시작한다. 고칠 `src/cmd/init/merge.rs`와 시험 도구가 그 PR에 있다.

## 5. 범위 밖

- 다른 base로 가는 `chute/setup` PR을 찾아 알리는 것. `gh pr list --base <기본 브랜치>`는 그대로다. 그 브랜치는 3절의 "열린 PR 없이 남은 브랜치"로 다룬다.
- 보고서 5(`gh`의 기본 저장소와 `info.remote_name`의 저장소가 다를 때). 재현하지 못했고 코드베이스 전체가 `gh -R`을 쓰지 않는다.
- 큐(batch)의 머지 경로. 이 플랜은 `chute init --setup`의 직접 머지만 고친다.
- `docs/plans/chute-setup.md`. 플랜 문서는 고치지 않는다. 바뀐 결정은 이 문서에 적었다.

## 6. 사용자가 정해야 하는 결정

검증을 약하게 하는 변경은 없다. 기본 브랜치에 들어가는 트리는 지금과 같이 검증한 트리뿐이고, 이력 조건이 더해진다.

- **열린 setup PR에 남이 올린 커밋은 `--yes`에서도 새 setup 커밋으로 덮어쓴다(기본안).** 지금은 `chute.toml`이 같고 다른 파일만 더한 HEAD는 ②에서 거절되고, `chute.toml`이 다르면 덮어쓴다. 기본안은 둘 다 덮어쓰고 덮어쓴 SHA를 출력한다. 다른 안은 열린 PR의 HEAD가 chute가 만든 모양(부모 하나, setup 파일만 바꿈)이 아니면 `--yes`에서 멈추는 것인데, 4의 실행 비트 HEAD("새로 만듦")와 충돌한다. 기본안으로 가려면 따로 정할 것이 없다.
- **열린 PR 없이 남은 `chute/setup`은 `--yes`에서 멈춘다(기본안).** 터미널에서는 SHA를 보이고 `y`로 덮어쓴다. 알리기만 하고 `--yes`에서 덮어쓰는 안은 사용자 커밋이 확인 없이 사라질 수 있어 고르지 않았다.
