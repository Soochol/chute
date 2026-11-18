# 미뤄 둔 후속 세 건 플랜: 다시 돌림 기록(#21), 단독 반송 보고서 문구(#22), 허브 오류 중복(#24)

> 작성일 2026-11-19. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 "미뤄둔것도 진행"이라고 했다. orch ask 목록에서 미뤄 둔 #21, #22, #24 세 건이다. 셋 다 앞선 PR의 리뷰가 찾아 재현했지만, 그 PR의 성공 조건을 깨지 않고 기준 브랜치에서 되던 동작도 깨지 않아 고치지 않고 남긴 발견이다. 기준은 main `d62526c`이다.

| 번호 | 출처 | 문제 | 재현 근거 |
|---|---|---|---|
| #21-1 | PR #104(solo-retry) 미해결 1 | 묶음을 나눠 찾는 경로에서 앞선 PR이 이미 머지돼 기준이 움직이면, `convict`가 `check_alone`과 `base_sound`(기준 확인)로 검증을 두 번 더 돌린 뒤 `rerun`을 부른다. `Rerun.failed_round`는 그때의 `self.record.verifications`라서 실패한 회차가 아니라 기준 확인 회차를 가리킨다 | 리뷰어 시험 `zz_bisect_middle_pr_flakes_rerun`(PR 1·2·3 가운데 2가 흔들림): `failed_round: 6`, 실제 실패 회차는 5 |
| #21-2 | PR #104 미해결 2 | 다시 돈 검증이 통과하면 흔들림을 실패한 트리(`Rerunning.failed_tree`)로 기록한다. 다시 돈 검증은 병합 커밋을 새로 만들어 SHA가 다르다. batch 끝 `gather_flaky_evidence`는 `record.pushed.contains(&entry.tree)`일 때만 probe를 돌리므로 이 흔들림은 probe되지 않는다 | 리뷰어 시험 `zz_rerun_flaky_with_probe_slow_verify`: `probes=[]` |
| #21-3 | PR #104 미해결 3 | `flaky::rerun_signal`이 `Vec::contains`로 두 시도의 이름을 맞대어 이름 수의 제곱으로 느려진다. 이름 1만 개에 1.6초, 10만 개에 160초 | 리뷰어 시험 `tests/rerun_probe.rs` |
| #22 | PR #105(solo-report) 미해결 1~4 | 단독 반송 보고서의 `범위`와 `기준 브랜치` 줄이 여러 단계나 시간 초과가 얽힌 드문 경우에 부정확하다(아래 표) | 리뷰어 시험 `tests/zz_repro.rs`(지워짐). 조건은 보고서 `/home/dev/.claude/orch/reports/solo-report/solo-report.md`에 있다 |
| #24 | PR #107(board-a-todo) 리뷰 발견 4 | 닿지 않는 허브의 오류 글이 Needs you 카드(`todoOf`의 `hub` 항목 `detail`)와 저장소 머리 아래(`main.js` `repoView`의 `repo.hub.error`)에 두 번 나온다 | 리뷰어 스크립트 `.repro/findings.mjs hub-twice` |

#22의 네 경우. 요청은 "원래 기록은 4가지라 했으나 3가지만 적혀 있다"고 했다. solo-report 보고서의 "미해결 사항"에 넷이 다 있고, 요청의 첫 항목("시간 초과·실패가 섞인 두 번 돌림의 표기")이 보고서의 1과 2를 한 줄로 묶은 것이다. 빠진 넷째는 없다.

| 보고서 번호 | 경우 | 지금 쓰는 말 | 빠지거나 틀린 사실 |
|---|---|---|---|
| 1 | 첫 검증이 실패하고(두 시도가 다른 시험) 다시 돈 검증이 시간 초과 | `시간 초과로 끝났다(2번 돌려 2번 시간 초과)` | 한 번은 실패였다 |
| 2 | 첫 검증이 시간 초과이고 다시 돈 검증이 실패 | `실패했다(2번 돌려 2번 실패)` | 한 번은 시간 초과였다 |
| 3 | 단계 `check`, `lint` 순서에서 `lint`가 다시 돌림을 일으키고 다시 돈 검증이 `check`에서 실패 | `실패했다(1번 돌려 1번 실패)` | 같은 기준에서 앞선 검증 때 `check`가 통과했다 |
| 4 | 이 batch가 push한 커밋이 뒤 PR의 단독 기준이 되고, 그 커밋의 검증은 `paths`로 실패한 단계를 건너뛰었다 | `- 기준 브랜치: 이 batch에서 돌려 통과했다` | 그 단계는 기준에서 돌지 않았다 |

## 2. 확정된 결정

요청(`/home/dev/.claude/orch/briefs/plan-chute-deferred/plan-chute-deferred-20261119-080635.md`)에 적힌 것이다.

| 결정 |
|---|
| #21의 세 건, #22의 문구 경우들, #24의 중복을 모두 이번에 처리한다 |
| #24는 오류 글이 한 번만 나오게 한다 |
| mod 변경은 병합 뒤 `cargo install --path . --locked`와 `chute install`로 설치본에 반영한다. 오케스트레이터가 한다(작업자는 하지 않는다) |
| 시간 문제 재현은 결정적 지연 주입만 쓴다. PC 전체 부하는 걸지 않는다 |

## 3. 스스로 정한 결정과 이유

### 3.1 작업 나눔

| 결정 | 이유 |
|---|---|
| 작업 셋: `retry-fix`(#21), `report-fix`(#22), `board-hub-once`(#24) | 셋은 고치는 곳이 다르고 각자 한 PR로 검토할 수 있다 |
| `report-fix`는 `retry-fix` 뒤(`after`) | 둘 다 `src/engine.rs`의 `convict`·`rerun`·`EarlierRun`·`solo_evidence`를 다시 짠다. `retry-fix`는 다시 돌림을 일으킨 실패의 회차를 `convict`에서 `rerun`까지 넘기고, `report-fix`는 같은 자리에서 앞선 검증의 단계별 결과를 넘긴다. 같은 함수를 다시 짜므로 순차다. `tests/engine.rs`의 같은 시험 무리(`scripted_failures` 근처)에도 둘 다 시험을 더한다 |
| `board-hub-once`는 앞의 둘과 병렬 | `mod/chute/` 파일만 고치고 Rust 쪽과 겹치는 파일이 없다 |
| 방식: `retry-fix`는 poteto | batch 기록(`BatchRecord.reruns`)과 batch 끝 probe 대상을 정하는 검증 쪽 코드다 |
| 방식: `report-fix`는 poteto | `state.json`의 `last_failure`와 보고서 `.json`에 저장되는 `SoloEvidence`·`BaseCheck` 모양을 더하고 `engine.rs`의 근거 수집을 고친다(상태 저장·데이터) |
| 방식: `board-hub-once`는 basic, profile `minor-fix` | 화면에서 한 줄을 빼는 조정이고 파일 하나와 화면 확인으로 끝난다 |

### 3.2 다시 돌림 기록 (작업 `retry-fix`)

| 결정 | 이유 |
|---|---|
| `Rerun.failed_round`의 뜻은 그대로("다시 돌림을 일으킨 실패의 검증 회차", `src/model.rs` 주석)이고 값을 고친다. 값은 `rerun`에 넘긴 `failed`를 낸 검증의 회차, 즉 그 실패 로그가 있는 `verify-<회차>` 폴더 번호다 | 필드 이름과 뜻을 바꾸지 않으면 옛 batch 기록과 읽는 쪽이 그대로다. 나눠 찾는 경로에서 `failed`는 `check_alone`의 실패이거나 처음 합친 트리의 실패이고, 어느 쪽이든 회차는 그 실패를 낸 검증이 안다 |
| 회차는 엔진 안에서 실패와 함께 넘긴다(`Run::verify`가 회차를 함께 돌려주거나 `Alone::Fail`이 회차를 갖는 식). `verify::Failed`에는 필드를 더하지 않는다 | `verify::Failed`는 preflight와 post_verify도 쓴다. 엔진의 기록 문제를 엔진 안에서 끝낸다 |
| 흔들림 기록의 트리(`Flaky.tree`, `Flaky` 이벤트의 `tree`)는 실패한 트리 그대로 둔다 | solo-confirm 플랜의 결정 F다. 장부의 뜻("이 트리에서 실패했다가 다시 돌려 통과했다")을 지킨다 |
| 다시 돈 검증이 통과해 그 PR이 push되면 그 흔들림을 "push됨"으로 보고 batch 끝 probe 대상에 넣는다. probe의 `flaked` 트리는 push된 커밋이다. 실패한 트리와 push된 커밋의 연결은 `Run` 안(메모리)에 둔다. 기록 필드가 꼭 필요하면 `serde(default)`로 더하기만 한다 | probe는 "main에 들어간 변경에서 이 시험이 흔들리는가"를 본다. 실패한 트리는 push되지 않아 main에 없는 커밋이고, push된 커밋이 같은 PR 변경을 담은 main의 커밋이다. 다시 돈 검증 뒤 main이 움직여 다시 만들었으면 실제로 push된 커밋이 대상이다. 다시 돈 PR이 push되지 않으면 지금처럼 probe하지 않는다 |
| 지금 두 저장소(chute, vallythevally)의 `[[verify]]`에는 `probe`가 없어 #21-2는 당장 동작을 바꾸지 않는다. 그래도 고친다 | 사용자가 미뤄 둔 항목을 진행하라고 했다. `probe`를 더하는 날 이 경로가 바로 쓰인다 |
| `rerun_signal`, `known_flaky`→`ledger_tests`, `Run::solo_evidence`의 이름 중복 제거를 집합(`HashSet`/`BTreeSet`)으로 바꿔 이름 수에 선형으로 만든다. 돌려주는 목록의 순서(첫 시도의 이름, 그 뒤 마지막 시도에만 있는 이름. `ledger_tests`는 입력 순서)는 그대로다 | 셋 다 같은 단독 실패 경로에서 같은 이름 목록을 맞댄다. 하나만 고치면 다음 자리가 같은 크기에서 느리다. 순서는 기존 시험과 batch 기록이 본다 |
| 시험은 시간을 재지 않는다. 10만 개 이름을 넣은 시험이 결과를 맞히는지만 보고, 작업자가 고치기 전과 뒤의 걸린 시간을 셸 `time`으로 재 보고서에 적는다 | `docs/testing.md`가 시험 코드의 `Instant::now`와 벽시계 판정을 막는다 |

### 3.3 단독 반송 보고서 문구 (작업 `report-fix`)

판정(언제 다시 돌리고 언제 반송하는지)은 바꾸지 않는다. 보고서가 확인한 사실만 정확히 적게 한다.

| 결정 | 이유 |
|---|---|
| `범위` 줄의 모양: `- 범위: 단독. 기준 브랜치에 {이 PR}만 얹은 트리에서 {결과}({N}번 돌려 {내역})`. `N`은 지금처럼 같은 기준에서 이 PR을 단독으로 검증한 횟수다 | 지금 모양을 지키면서 회차별 사실을 담는다 |
| `결과`: 이 단계가 실패한 회차가 모두 실패면 `실패했다`, 모두 시간 초과면 `시간 초과로 끝났다`, 둘이 섞이면 `통과하지 못했다` | 보고서 1·2의 경우. 섞였을 때 한쪽 말만 쓰면 다른 쪽이 거짓이 된다 |
| `내역`: 실패한 단계의 회차별 결과를 `{n}번 실패`, `{n}번 시간 초과`, `{n}번 통과`, `{n}번은 앞 단계가 실패해 이 단계를 돌리지 않았다` 순서로 0인 것을 빼고 `, `로 잇는다. 예: `2번 돌려 1번 실패, 1번 시간 초과`, `2번 돌려 1번 실패, 1번 통과`, `2번 돌려 1번 실패, 1번은 앞 단계가 실패해 이 단계를 돌리지 않았다`. 모두 같으면 지금 문구 그대로다(`1번 돌려 1번 실패`, `2번 돌려 2번 실패`, `2번 돌려 2번 시간 초과`) | 보고서 3의 경우. 검증은 단계를 차례로 돌리다 처음 실패한 단계에서 멈춘다(`verify::run`). 앞선 검증에서 다른 단계가 실패했으면 이 단계는 그 앞이라 통과했거나 그 뒤라 돌지 않았다. 둘을 가려 적는다 |
| 앞선 검증을 세는 조건을 "같은 단계, 같은 기준"에서 "같은 기준"으로 넓힌다. `시도별 실패 시험`(`attempts`)과 장부 조회는 지금처럼 이 단계가 실패한 회차의 시도만 쓴다 | 다른 단계의 시도를 섞지 않는다는 solo-report의 수정(발견 "다른 단계의 앞선 시도가 섞임")을 지키면서 회차 수만 바로잡는다 |
| 기존 시험 `a_solo_report_counts_only_the_attempts_of_the_step_that_failed`의 기대 `(1번 돌려 1번 실패)`는 `(2번 돌려 1번 실패, 1번은 앞 단계가 실패해 이 단계를 돌리지 않았다)`로 바뀐다. 그 시험의 다른 단언(실패 단계 `lint`, `test_a`·`test_b`가 없음, `attempts`가 `lint_x` 두 번)은 그대로 둔다 | 그 시험의 장면이 바로 보고서 3의 거울(앞 단계 `check`가 다시 돌림을 일으키고 다시 돈 검증이 `lint`에서 실패)이다. 기대 문구가 이번 결정 그대로 바뀌는 것이고 단언을 빼지 않으므로 검증을 약하게 하지 않는다 |
| `기준 브랜치` 줄: 기준이 이 batch가 push한 커밋일 때 그 커밋을 통과시킨 검증을 본다. 실패한 단계가 그 검증에서 돌아 통과했으면 지금 문구(`- 기준 브랜치: 이 batch에서 돌려 통과했다`). `paths`로 건너뛰었으면 `` - 기준 브랜치: 이 batch가 push한 커밋이다. 그 검증은 변경 범위(`paths`)에 들지 않아 이 단계를 건너뛰었다 ``. 그 검증을 통과 기록 재사용으로 갈음했으면 `- 기준 브랜치: 이 batch가 push한 커밋이다. 같은 트리를 앞서 검증한 기록이 있어 이 batch에서는 돌리지 않았다`. 기준을 이 batch에서 전체 검증(`check_base`)으로 돌렸으면 지금 문구 그대로다 | 보고서 4의 경우. 재사용으로 push한 커밋도 지금 `bases_ran`에 들어 같은 거짓 문구를 쓴다(`land`가 push한 커밋을 모두 넣는다). 같은 줄의 같은 결함이라 함께 고친다. 건너뛴 단계는 batch 기록의 `skipped`(회차별), 재사용은 `reused`에 이미 있다 |
| 저장 모양은 더하기만 한다: `SoloEvidence`에 회차별 결과 필드(`serde(default)`), `BaseCheck`에 두 값. 새 필드가 없는 옛 보고서 `.json`과 `state.json`의 `last_failure`는 읽히고 지금 문구로 그린다 | 저장소 규칙: 상태의 필드 이름과 뜻은 바꾸지 않고 더하기만 한다. `chute status --json`의 `FailureView`는 근거를 싣지 않아 상태 JSON은 바뀌지 않는다 |
| `render_solo_scope`의 "모든 시도에서 같았다" 비교도 집합으로 바꾼다 | #21-3과 같은 제곱 비교다(`pair[1].contains`). 이 함수를 이 작업이 다시 짜므로 여기서 고친다 |
| `skill/manual.md` 155줄의 `기준 브랜치` 설명에 "이 batch가 push한 커밋이라 다시 돌리지 않은 경우(건너뛴 단계, 재사용)"를 더한다 | 작업자가 보고서의 새 문구를 읽고 판단하는 안내서다 |
| 시간 초과 장면은 결정적 지연으로 만든다: 그 회차의 가짜 단계 스크립트가 실패 줄을 찍은 뒤 `sleep 600`하고, 단계 `timeout`은 다른 회차가 넘지 않을 만큼(예: 5초) 둔다. 기존 `a_pr_that_runs_past_the_time_limit_is_bounced_as_a_timeout`과 같은 방식이다 | 요청의 PC 부하 규칙. 엔진의 단계 실행은 벽시계(`Clock::System`)를 쓰므로 멈춘 시계를 넣을 수 없다. 지연이 한도를 크게 넘어 결과가 갈리지 않는다 |

### 3.4 허브 오류 한 번만 (작업 `board-hub-once`)

| 결정 | 이유 |
|---|---|
| 오류 글은 Needs you의 `Hub unreachable` 카드에만 둔다. 저장소 머리 아래에서는 `repo.hub.error` 글을 빼고 `Retrying every 5s` 줄은 남긴다 | 닿지 않는 허브는 할 일이라 Needs you에 늘 오른다(`todoOf`). Needs you가 맨 위라 먼저 보인다. 저장소 안에는 그 저장소가 다시 읽기를 기다린다는 사실만 남긴다. 저장소 머리의 `hub <이름>` 빨간 칩과 상태 `Hub unreachable`도 그대로다 |
| `board.js`의 데이터(`hub.error`, `todo`의 `hub` 항목 `detail`)는 바꾸지 않는다 | 그리는 쪽 한 줄의 중복이다. 데이터 시험(`todo: ...`, `an unreachable hub shows its reason ...`)이 그대로 뜻을 지킨다 |

## 4. 작업

| 작업 | 방식 | 고치는 곳 | 순서 |
|---|---|---|---|
| `retry-fix` | poteto | `src/engine.rs`, `src/flaky.rs`, `tests/engine.rs` | 먼저 |
| `report-fix` | poteto | `src/engine.rs`, `src/model.rs`, `src/feedback.rs`, `src/feedback/tests.rs`, `src/cmd/preflight.rs`, `tests/engine.rs`, `skill/manual.md` | `retry-fix` 뒤 (같은 함수를 다시 짠다, 3.1) |
| `board-hub-once` | basic (minor-fix) | `mod/chute/main.js`, `mod/chute/board.js`, `mod/chute/board.test.js` | 병렬 |

`board-hub-once`가 병합되면 오케스트레이터가 `cargo install --path . --locked`와 `chute install`로 설치본에 반영한다.

## 5. 범위 밖

- 판정 바꾸기: `paths`로 실패한 단계를 건너뛴 기준을 단독 판정의 근거로 쓰는 것은 그대로다. 이번에는 보고서가 그 사실을 적게만 한다(6절).
- 같은 시험이 두 번 연속 실패하고 장부에 없는 경우의 다시 돌림(solo-confirm 6절 1번)은 이번 범위가 아니다.
- board-a-todo 보고서의 `running` 뜻(승인 대기 저장소의 배치를 실행 중으로 셀지)은 이번 요청에 없다.
- 단계에 `probe`를 더하는 설정 변경은 하지 않는다.

## 6. 사용자가 정해야 하는 결정

이번 작업에서 검증 단계를 빼거나, 시간 제한을 늘리거나, `paths`를 줄이는 등 검증을 약하게 하는 변경은 없다. 병합 전에 사용자가 정할 것은 없다.

나중에 정할 수 있는 것 하나: 단독 실패의 기준이 이 batch가 `paths`로 그 단계를 건너뛰고 push한 커밋이면, 지금은 그 기준을 다시 돌리지 않고 PR을 반송한다(보고서는 이번 작업 뒤 그 사실을 적는다). 그 단계를 기준에서 한 번 돌려 확인하게 할지는 판정을 바꾸는 일이라 이번에 하지 않았다. 원하면 별도 플랜으로 다룬다. 바꾸면 그런 반송마다 검증이 한 번 는다.
