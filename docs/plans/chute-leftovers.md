# 미뤄 둔 후속 네 묶음 플랜: 단독 보고서 0회(#26), status 시간 값(#27), 머지 뒤 검사 줄(#28), 상세 창 사건(#29)

> 작성일 2026-11-19. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 "미뤄둔거 진행"이라고 했다. orch ask 목록에서 미뤄 둔 #26~#29다. 모두 앞선 PR(#114, #116, #117, #118)의 리뷰나 화면 확인이 찾았지만, 그 PR의 성공 조건을 깨지 않아 고치지 않고 남긴 것이다. 기준은 main `4372d38`이다.

| 번호 | 출처 | 문제 | 근거 |
|---|---|---|---|
| #26-1 | PR #114(report-fix) 미해결 2 | 옛 근거에 `runs: 0`이 있고 `run_outcomes`가 비면 단독 반송 보고서의 `범위` 줄이 `실패했다(0번 돌려 )`가 된다. 엔진은 `runs >= 1`만 쓰므로 손으로 고쳤거나 깨진 옛 보고서에서만 생긴다 | `src/feedback.rs`의 `solo_runs`가 빈 `outcomes`에 `outcomes.len()`과 빈 `parts`를 그대로 잇는다 |
| #26-2 | PR #114 미해결 1 | no-comments 리뷰어가 문서 주석 4건(`PassedBy`, `name_pushed_base`, `earlier_outcome`, `solo_runs`) 삭제를 권했지만 남겼다 | `src/engine.rs` 958, 1912, 1985줄, `src/feedback.rs` 899줄 |
| #27-1 | PR #116(board-e-data) 미해결 1 | `batch.step_started_ms`가 `step`이 `None`인 동안(push, 회차 사이)에도 마지막 단계의 시작 시각으로 남는다 | `src/engine.rs`의 `set_phase`가 `step`만 비운다 |
| #27-2 | PR #116 미해결 2 | `usual`의 중앙값이 `u64::MAX`급 값 둘을 더하다 overflow로 panic한다. `today.median_lead_s`는 같은 식을 따로 적었다 | `src/cmd/status.rs`의 `median`과 `today` 끝의 `match leads.len()` |
| #27-3 | PR #116 미해결 3 | 첫 회차에 반송된 단독 batch가 `usual.batch_s` 표본에 든다 | 표본 조건이 `verifications == 1 && ran == steps`뿐이다 |
| #27-4 | PR #116 미해결 4 | 55284ce가 쓴 `batch.json`은 `ran`이 비어 있다 | 그 커밋은 main에 들어간 적이 없다 |
| #28 | PR #117(board-e-show) 미해결 | 패널 300px에서 머지 뒤 검사 줄 `5a4b3c2 Checking · e2e`가 오른쪽 `5m 22s / usually 7m 7s`에 밀려 `Checking…`으로 잘린다 | `mod/chute/main.js` `postVerifyView`의 왼쪽 글이 `truncate()`다 |
| #29-1 | PR #118(board-f-detail) 미해결 1 | Events 탭이 history의 읽지 못한 줄 경고(stderr)에도 `Showing the latest 2000 events`를 띄운다 | `mod/chute/detail.js` `parseEvents`의 `cut`이 "stderr가 비지 않음"이다. `chute events`는 깨진 줄 경고와 잘림 경고를 둘 다 stderr에 쓴다(`src/cmd/events.rs`) |
| #29-2 | PR #118 미해결 2 | `pr`가 빠진 `merged`, `bounced`, `submitted` 사건이 `#undefined`로 나온다 | `detail.js` `EVENT`의 세 문장이 `#${e.pr}`을 그대로 쓴다 |
| #29-3 | PR #118 확인 항목 | 같은 반송 이유가 Overview에서 `head_changed`, Events에서 `head changed`로 다르게 보인다 | PR #118 되돌림 반영에서 이미 고쳤다(4절) |
| #29-4 | PR #118 남은 한계 | 키보드나 알림으로 처음 열면 마우스 원점이 없어 상세 창이 가운데에 열려 패널을 가릴 수 있다 | 4절 |

## 2. 확정된 결정

요청(`/home/dev/.claude/orch/briefs/plan-chute-leftovers/plan-chute-leftovers-20261119-104319.md`)에 적힌 것이다.

| 결정 |
|---|
| #26~#29를 이번에 처리한다. #27-4는 플랜에 "처리 안 함"으로 적기만 한다 |
| #23(running 수에 승인 대기 batch를 셀지)은 기본값으로 두고 이번 범위가 아니다 |
| mod 변경은 병합 뒤 오케스트레이터가 `cargo install`과 `chute install`로 반영한다. 본체 변경은 데몬 재시작이 필요하고 오케스트레이터가 한다. 작업자는 하지 않는다 |
| 시간 문제 재현은 결정적 지연 주입만 쓴다. PC 전체 부하는 걸지 않는다 |

## 3. 스스로 정한 결정과 이유

### 3.1 작업 나눔과 방식

| 결정 | 이유 |
|---|---|
| 작업 넷: `solo-zero-runs`(#26), `status-usual-fix`(#27), `board-post-wrap`(#28), `detail-events-fix`(#29) | 고치는 파일이 서로 겹치지 않고 각자 한 PR로 검토할 수 있다 |
| 넷 모두 병렬 | `solo-zero-runs`는 `src/feedback.rs`와 `src/feedback/tests.rs`, `status-usual-fix`는 `src/engine.rs`·`src/model.rs`·`src/cmd/status.rs`·`tests/engine.rs`, `board-post-wrap`은 `mod/chute/main.js`, `detail-events-fix`는 `mod/chute/detail.js`·`mod/chute/detail.test.js`만 고친다. #26-2의 결정(3.2)으로 `solo-zero-runs`는 `src/engine.rs`를 고치지 않고, #29-4의 결정(4절)으로 `detail-events-fix`는 `main.js`를 고치지 않는다 |
| `solo-zero-runs`: basic, `minor-fix` | 보고서 글 한 줄을 그리는 함수와 시험 하나다. 판정이나 저장 모양은 바꾸지 않는다 |
| `status-usual-fix`: poteto | `state.json`의 `batch`에 저장되는 값(`step_started_ms`)을 엔진이 쓰는 시점을 바꾼다(상태 저장) |
| `board-post-wrap`: basic, `minor-fix` | 화면 한 줄의 배치 조정이다 |
| `detail-events-fix`: basic, `minor-fix` | 화면의 순수 데이터 함수 두 곳과 시험이다 |

### 3.2 단독 보고서 (작업 `solo-zero-runs`, #26)

| 결정 | 이유 |
|---|---|
| 회차별 결과가 하나도 없으면(`run_outcomes`가 비고 `runs`가 0) `범위` 줄은 결과 말만 쓰고 괄호를 붙이지 않는다: `- 범위: 단독. 기준 브랜치에 {이 PR}만 얹은 트리에서 실패했다`(시간 초과면 `시간 초과로 끝났다`) | 몇 번 돌렸는지 모르는데 `0번`이나 `1번`을 지어 쓰면 거짓이다. 이 PR이 이 단계에서 실패했거나 시간 초과였다는 사실은 근거가 없어도 보고서 머리가 이미 안다(`timed_out`). 근거가 아예 없을 때의 문구(`같은 단계가 실패했다`)보다 정확하다 |
| `runs`가 1 이상인 옛 근거와 새 근거의 문구는 그대로다 | 시험 `an_old_solo_evidence_without_runs_by_kind_still_renders_as_before`와 report-fix의 시험들이 지킨다 |
| 문서 주석 4건은 모두 남긴다. `solo_runs`의 주석만 새 경우(결과가 없으면 괄호 없음)를 담게 고친다 | 저장소 규칙(AGENTS.md)은 코드 주석을 영어로 쓰라고만 정하고 문서 주석을 막지 않는다. `src/engine.rs`(19개), `src/model.rs`(42개)도 타입과 함수에 `///`를 단다. 넷은 이름에서 알 수 없는 것을 적는다: `PassedBy`는 "마지막으로 통과한 검증"이 이 batch의 회차였는지 재사용이었는지, `name_pushed_base`는 언제만 값을 바꾸는지(이 batch가 push했고 기준으로 돌리지 않은 커밋), `earlier_outcome`은 왜 단계 순서로 결과를 추론할 수 있는지(검증은 처음 실패한 단계에서 멈춘다), `solo_runs`는 출력 모양과 옛 근거의 처리다 |

### 3.3 status의 시간 값 (작업 `status-usual-fix`, #27)

| 결정 | 이유 |
|---|---|
| 엔진이 지키는 불변식: `batch.step`이 `None`이면 `batch.step_started_ms`도 `None`이다. `step`을 비우는 자리(`set_phase`, 회차 시작)가 둘을 함께 비운다. 회차 시작은 이미 그렇게 하므로 `set_phase`를 고친다. 단계가 끝난 뒤 다음 일을 시작하기 전까지 `step`과 그 시작 시각이 함께 남는 것은 그대로 둔다 | 근본 원인은 `step`을 비우는 자리가 `step_started_ms`를 함께 비우지 않는 것이다. 둘이 함께 남아 있으면 화면은 그 단계를 지금 단계로 그릴 뿐 어긋난 값을 그리지 않는다. 엔진에서 고치면 `status --json`, `state.json`을 읽는 다른 쪽이 따로 거르지 않아도 된다. 화면(`board.js`)은 이미 `step`이 있을 때만 경과를 그리고 그 시험이 있다 |
| 필드 이름은 그대로이고 뜻은 원래 주석("`step`의 단계가 시작한 때")에 맞춘다. `src/model.rs`의 주석 `Cleared when a round starts.`는 새 불변식으로 고친다. `tests/fixtures/status-full.json`(step과 `step_started_ms`가 함께 있다)과 `mod/chute/`는 고치지 않는다 | 저장소 규칙: 상태 JSON 필드의 이름과 뜻은 바꾸지 않는다. 이번 변경은 남은 값을 지우는 것이라 뜻이 바뀌지 않는다 |
| `median`은 두 가운데 값의 평균을 `a / 2 + b / 2 + (a % 2 + b % 2) / 2`처럼 overflow 없이 구한다. 값은 지금과 같다(정확한 `(a + b) / 2`의 버림) | 기존 시험의 기대(`median_lead_s` 250 등)가 그대로다 |
| `today.median_lead_s`도 같은 `median` 함수를 부른다 | 같은 규칙이 두 곳에 따로 있으면 한쪽만 고쳐진다. `leads`는 초 단위라 지금 값으로는 overflow하지 않지만 한 함수로 모은다 |
| `usual.batch_s` 표본은 "한 회차로 끝나고, 반송한 PR이 없고, 머지한 PR이 있고, 첫 회차 단계 목록이 지금 batch와 같은" `batch_finished`다(`verifications == 1 && bounced.is_empty() && !merged.is_empty() && ran == steps`) | 패널은 `usual.batch_s`를 "이 단계 구성의 batch가 평소 끝까지 통과하는 시간"으로 쓴다(`/ usually`, 진행 비율, `Slower than usual`). 반송된 batch는 실패한 단계에서 멈춰 그 시간보다 짧다. 표본에 들면 중앙값이 내려가 정상 batch가 느리다고 보인다. 마지막 단계까지 가서 반송됐어도 그 단계를 끝까지 돌지 않았다 |
| `usual.steps`와 `usual.post_verify_s`의 표본은 그대로다 | `step_times`는 이미 `passed`로 끝난 실행만 history에 쓴다. 머지 뒤 검사는 `post_green`만 센다 |
| #27-4(55284ce의 빈 `ran`)는 처리하지 않는다 | 그 커밋은 board-e-data 브랜치 안에서만 있었고 main에 들어간 적이 없다. 그 코드로 쓴 `batch.json`이 있을 수 없다 |

### 3.4 머지 뒤 검사 줄 (작업 `board-post-wrap`, #28)

| 결정 | 이유 |
|---|---|
| `postVerifyView`의 줄에서 왼쪽 글을 자르지 않고 줄바꿈한다(`truncate()` 대신 `whitespace_normal()`). 오른쪽 시간 글은 지금처럼 `flex_none`으로 한 줄을 지킨다 | 300px에서 둘 다 다 읽히려면 한쪽이 두 줄이 되어야 한다. 커밋·단계 이름은 길이가 일정하지 않고 시간 글은 짧고 모양이 정해져 있다. board-e-show가 단계 줄을 같은 방식(잘리지 않고 줄바꿈)으로 했다 |
| 시간 글을 줄이거나(`/ usually` 빼기) 아래 줄로 내리지 않는다 | 넓은 폭에서도 바뀌는 안이다. 좁을 때 왼쪽만 한 줄 더 쓰는 것이 바뀌는 곳이 가장 적다 |
| 같은 `row`를 쓰는 통과 줄(`<sha> passed`)도 같이 줄바꿈된다 | 같은 함수다. 그 줄은 짧아 실제로는 한 줄이다 |

### 3.5 상세 창 사건 (작업 `detail-events-fix`, #29)

| 결정 | 이유 |
|---|---|
| `parseEvents`의 `cut`은 stderr가 비지 않고, stdout의 비지 않은 줄 수(읽은 사건과 건너뛴 줄의 합)가 `DETAIL_EVENTS` 이상일 때만 참이다 | `chute events`는 잘랐을 때 정확히 `--limit`개를 낸다. 그보다 적으면 잘리지 않았다. stderr 문구(한국어)를 맞대 읽지 않으므로 본체 문구가 바뀌어도 깨지지 않고 mod에 한국어를 두지 않는다. 사건이 정확히 2000개이면서 깨진 줄 경고가 있을 때만 잘렸다고 잘못 말한다. 그때도 보이는 사건은 맞다 |
| `pr`가 양의 정수가 아닌 `merged`, `bounced`, `submitted` 사건은 `#번호`를 빼고 쓴다(`merged`, `bounced · head changed · attempt 1`, `submitted`). 다른 사건이 쓰는 `withPr`와 같은 규칙이다 | 본체는 늘 `pr`를 보내므로 깨진 입력에서만 생긴다. 없는 값을 `#undefined`로 보이지 않는다 |
| Overview의 `Bounce reasons` 줄도 `cut`을 쓰므로 같이 고쳐진다. `main.js`는 고치지 않는다 | `main.js`는 `data.events.cut`만 읽는다 |

## 4. 처리하지 않는 것

| 번호 | 이유 |
|---|---|
| #27-4 | 3.3. main에 들어간 적 없는 커밋이다 |
| #29-3 | PR #118의 되돌림 반영에서 `bounceReasons`가 Events와 같은 `spaced`를 쓰게 고쳤고 시험 `a bounce reason reads the same in the overview and in the events`(`mod/chute/detail.test.js`)가 지킨다. main `4372d38`에서 확인했다 |
| #29-4 | 지금 상세 창을 여는 길은 `Details ›`와 `Ledger ›` 링크의 클릭뿐이다. `main.js`에는 키 바인딩(`bind_keys`, `on_action`)이 없고, 알림은 `open-file`만 연다. 링크를 누르면 루트의 `on_mouse_down`이 먼저 원점을 적으므로 이 한계에 닿는 길이 없다. 패널 원점을 마우스 없이 읽는 API도 없다(`herdr.d.ts`의 `placement()`는 떠 있는 mod에만 값이 있다). 키보드나 알림으로 여는 길을 더하는 작업이 그때 위치를 정한다 |

## 5. 작업

| 작업 | 방식 | 고치는 곳 | 순서 |
|---|---|---|---|
| `solo-zero-runs` | basic (minor-fix) | `src/feedback.rs`, `src/feedback/tests.rs` | 병렬 |
| `status-usual-fix` | poteto | `src/engine.rs`, `src/model.rs`, `src/cmd/status.rs`, `tests/engine.rs` | 병렬 |
| `board-post-wrap` | basic (minor-fix) | `mod/chute/main.js` | 병렬 |
| `detail-events-fix` | basic (minor-fix) | `mod/chute/detail.js`, `mod/chute/detail.test.js` | 병렬 |

`status-usual-fix`가 병합되면 오케스트레이터가 데몬을 다시 띄운다. `board-post-wrap`, `detail-events-fix`가 병합되면 `cargo install --path . --locked`와 `chute install`로 설치본에 반영한다.

## 6. 범위 밖

- #23(running 수에 승인 대기 batch를 셀지).
- 상세 창을 키보드나 알림으로 여는 길을 더하는 것(4절 #29-4).
- 360px 패널의 repo 머리 줄이 hub 칩과 겹치는 것(PR #118 미해결 첫 줄). 이번 요청 목록에 없다.
- `detailSpot`이 정수가 아닌 패널 x에서 소수 폭을 낼 수 있는 것(PR #118 남은 한계). 이번 요청 목록에 없다.

## 7. 사용자가 정해야 하는 결정

이번 작업에서 검증 단계를 빼거나, 시간 제한을 늘리거나, `paths`를 줄이는 등 검증을 약하게 하는 변경은 없다. 병합 전에 사용자가 정할 것은 없다.

`usual.batch_s`에서 반송된 batch를 빼면 반송이 잦은 단계 구성에서는 표본 3개가 모이는 데 더 오래 걸려 그동안 평소 시간이 나오지 않는다(지금도 표본이 3개 미만이면 나오지 않는다). 반송 batch도 세기를 원하면 알려 주면 된다.
