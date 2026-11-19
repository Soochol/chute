# 현황판 개선안 2 플랜: 시간축(D), 본체 데이터(E), 상세 창(F)

> 작성일 2026-11-19. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

board-ux2 조사가 현황판 개선안 2를 artifact(https://claude.ai/artifact/5GiHNDPsr4UfbZomupSMbC, 사본 `/home/dev/.claude/orch/reports/board-ux2/chute-board-ux2.html`)로 보였다. 묶음 A·B·C는 PR #107·#108·#109로, 허브 오류 중복 수정은 #111로 머지됐다(플랜 `docs/plans/board-abc.md`, `docs/plans/chute-deferred.md`). 사용자는 "D E F도 진행해줘"라고 했다. 기준은 main `c7ebb75`이다(`mod/chute/main.js` 1384줄, `board.js` 981줄, `board.test.js` 2270줄).

개선안이 짚은 문제 가운데 이 플랜이 푸는 것:

| 번호 | 문제 | 근거 | 묶음 |
|---|---|---|---|
| 1 | 배치의 `usually`가 단계 구성이 다른 batch까지 섞은 중앙값이다. B가 60초 아래를 빼서 줄였지만 단계를 건너뛴 긴 batch와 다 돈 batch는 아직 섞인다 | `board.js` `usualSeconds`, `MIN_USUAL_SECONDS` | E(끝냄) |
| 4 | 단계 막대에 단계별 시간, 건너뛴 단계, 다시 돌림이 없다 | B는 status의 `steps`·`step`만으로 그렸다. `BatchRecord`에는 합계 `verify_s`뿐 | E |
| 6 | 머지 뒤 검사에 평소 시간과 진행 비율이 없다 | C는 막대만 그렸다. `post_verify.run`에는 `started_ms`와 `step`뿐 | E |
| 8 | 시간축이 없다. 언제 몰렸는지, 어느 batch에서 반송이 났는지, 같은 batch로 들어간 PR이 무엇인지 안 보인다 | `recent[]`와 `recent_merged[].batch`가 이미 status에 있다 | D |
| — | 장부와 타임라인은 패널 폭(300~360px)에 다 넣지 못한다 | 개선안 ② L2 | F |

## 2. 확정된 결정

요청(`/home/dev/.claude/orch/briefs/plan-board-def/plan-board-def-20261119-090132.md`)과 그것이 가리키는 개선안 ⑤·⑥에 적힌 것이다.

| 번호 | 결정 |
|---|---|
| Q4 | 배치 카드는 안 2(단계 막대)이고 본체 작업 E를 한다. 단계별 시간, 건너뛴 단계, 같은 단계 구성끼리의 평소 시간을 넣는다 |
| Q5 | 상세 창(F)을 만든다 |
| Q6 | 이번 묶음은 D, E, F. 권장 순서 D → E → F |
| D | 패널의 막대 줄(`recent[]` 10개), Recent를 batch로 묶기, 520px 이상 두 단. mod만 고친다 |
| E | batch 기록에 단계별 시간, status에 skipped·reruns·reused·단계별 평소 시간·머지 뒤 평소 시간·데몬 `doing`. C의 Post-merge 막대 평소 시간과 B의 단계 막대 시간도 E 데이터로 채운다. 상태 저장 형식에 필드가 는다(poteto) |
| F | 상세 창: Overview(큰 타임라인), Flaky, Events, Batches 탭. `mod/chute/`에 새 파일(`detail.js`와 시험), `main.js` |
| — | E는 `src/engine.rs`·`src/model.rs`를 고치므로 지금 도는 `retry-fix`와 그 뒤 `report-fix`(플랜 `docs/plans/chute-deferred.md`)와의 순서를 정해 `after`에 적는다 |
| — | 화면 문구는 영어(`AGENTS.md`, 2026-11-17 사용자 결정). gpui-kit 제약을 지킨다 |
| — | 시간 문제 재현은 결정적 지연 주입만. PC 전체 부하 금지 |
| — | mod 변경은 병합 뒤 오케스트레이터가 `cargo install --path . --locked`와 `chute install`로 설치본에 반영한다 |

## 3. 스스로 정한 결정과 이유

### 3.1 작업 나눔과 순서

| 결정 | 이유 |
|---|---|
| 작업 넷: `board-d-time`(D), `board-e-data`(E의 본체), `board-e-show`(E의 화면), `board-f-detail`(F) | E는 개선안이 "대, 위험 높음"이라 했다. 본체의 기록·status 필드와 mod의 그리기를 한 PR에 넣으면 검토 범위가 두 언어, 두 영역이다. 저장소 규칙상 status 필드를 **더하는** 것은 mod와 같은 PR일 필요가 없다(이름이나 뜻을 바꿀 때만 같은 PR). 본체가 먼저 필드를 내고 mod가 다음 PR에서 쓴다 |
| `board-d-time`은 먼저, 다른 플랜의 작업과 병렬 | mod만 고친다. `retry-fix`·`report-fix`는 Rust만 고쳐 겹치는 파일이 없다 |
| `board-e-data`는 `retry-fix`, `report-fix` 뒤 | 셋 다 `src/engine.rs`와 `tests/engine.rs`를 고치고, `report-fix`는 `src/model.rs`와 `src/cmd/preflight.rs`도 고친다. 특히 `report-fix`는 단독 반송 근거를 위해 "검증 회차마다 단계별 결과(통과·실패·시간 초과·돌지 않음)"를 `Run` 안에서 넘기게 고치고, `board-e-data`는 같은 `Run::verify` 자리에서 단계마다 결과와 걸린 시간을 기록한다. 같은 함수를 다시 짜므로 순차다. `report-fix`가 만든 회차별 단계 결과가 있으면 `board-e-data`가 그것을 기록에 쓴다(중복 구조를 만들지 않는다) |
| `board-e-show`는 `board-e-data`와 `board-d-time` 뒤 | 앞쪽은 쓰는 데이터를 만든다(한쪽이 다른 쪽 결과를 쓴다). 뒤쪽은 같은 세 mod 파일을 고치고, 특히 `board.test.js`의 `random malformed status never breaks the board` 시험의 `CONTAINERS` 한 줄을 둘 다 고친다. `board-e-data`가 `report-fix` 뒤라 늦게 시작하므로 D를 기다려도 늦어지지 않는다 |
| `board-f-detail`은 `board-d-time` 뒤, `board-e-show`와 병렬 | F의 `Details ›` 입구가 D가 만드는 막대 줄 제목 줄에 붙는다(같은 화면 영역). F와 E-show는 `main.js`만 함께 고치고 고치는 자리가 다르다(F: `init`·`render`의 창 갈래, 막대 줄 제목, 흔들림 줄 / E-show: `batchView`, `postVerifyView`, `repoHead`의 데몬 점). 두 작업의 `shared`에 `mod/chute/main.js`를 적고 최소 변경으로 하라고 지시서에 적는다. F는 `board.js`와 `board.test.js`를 고치지 않는다 |
| 방식: `board-d-time`은 basic | 화면 조정과 순수 데이터 함수 몇 개다. 상태 저장·동시성·검증 판정에 닿지 않는다 |
| 방식: `board-e-data`는 poteto | `state.json`의 `batch`, `batch.json`의 `BatchRecord`, `history.jsonl`의 `batch_finished` 사건에 필드가 늘고(상태 저장·데이터), 검증 실행부(`verify.rs`)의 콜백을 고친다 |
| 방식: `board-e-show`는 basic | status에서 받는 값을 보일 뿐인 화면 조정이다 |
| 방식: `board-f-detail`은 poteto | 새 화면(창)과 새 모듈(`detail.js`)을 설계하고 설치 코드(`src/cmd/install.rs`)의 설치 파일 목록을 고친다. 여러 모듈에 걸친 새 설계다 |

### 3.2 공통

| 결정 | 이유 |
|---|---|
| 기존 시험의 기대가 새 뜻 때문에 바뀌면 그 시험의 기대값이나 그 시험 안의 입력만 고친다. 단언을 빼지 않는다. 고친 시험 이름과 이유를 보고서에 적는다 | board-abc 3.1과 같다. 뜻이 바뀐 곳을 리뷰가 시험 diff로 본다 |
| mod 작업마다 격리 창 스크린샷을 찍는다. 방법은 `docs/plans/board-redesign.md` 4절과 board-narrow가 실제로 쓴 방법(`/home/dev/.claude/orch/reports/board-narrow/board-narrow.md`의 "화면 확인")이다. 폭은 샌드박스 레이아웃의 chute 블록 px 폭으로 정한다(560px도 같은 방법) | board-abc와 같은 확인 방법이다. 저장소의 `mod/chute/mod.json`(패널 360)은 바꾸지 않는다 |
| 새 mod 파일은 F의 `detail.js` 하나뿐이고, F가 `chute install`의 설치 목록(`src/cmd/install.rs`의 `MOD_FILES`)과 그 시험에 함께 더한다 | `chute install`은 목록에 있는 파일만 설치본에 넣는다. 목록에 없으면 설치본의 `main.js`가 `./detail.js`를 찾지 못한다 |

### 3.3 시간축 (작업 `board-d-time`)

| 결정 | 이유 |
|---|---|
| 저장소마다 `Last batches  N` 구역을 둔다. `recent[]`(최근 batch 10개, 새것이 앞)를 옛것부터 왼쪽에 막대 하나씩 그린다. 높이는 걸린 시간 `seconds`의 로그 눈금(가장 긴 batch가 가득, 가장 낮아도 보이는 높이), 색은 반송이 있는 batch가 `ansi1`, 머지만 있는 batch가 `ansi2`. 막대 아래 왼쪽에 가장 옛 batch의 끝난 시각, 오른쪽에 가장 새 batch의 끝난 시각(`clockText`) | 개선안 "24시간 막대 줄"을 status만으로 그린다. status의 `recent[]`는 10개뿐이라 제목을 `LAST 24H`가 아니라 `Last batches`로 한다. 24시간 전체는 F의 Overview가 맡는다 |
| 막대마다 tooltip: `HH:MM · 7m 55s · merged #616 #617 · bounced #608`(없는 조각은 뺀다) | 막대 hover는 `tooltip`으로(개선안 gpui-kit 제약) |
| `board.js`가 저장소에 `strip: { bars: [{ key, height, tone, tip }], from, to } | null`을 더한다. `recent[]`에서 `seconds`와 `finished_ms`가 수가 아닌 항목은 뺀다. 남은 것이 없으면 `null` | 데이터는 `board.js`에서 순수 함수로 만들고 시험한다(지금 구조) |
| Recent를 batch로 묶는다: 지금처럼 맨 앞 3줄(펼치면 전부)을 고른 뒤, 연이은 같은 batch 줄을 한 묶음으로 둔다. 묶음 머리 `00:34 · batch of 2 · 7m 55s`. 시각과 걸린 시간은 같은 id의 `recent[]` 항목에서, 없으면 시각은 그 묶음 첫 줄의 머지 시각이고 걸린 시간은 뺀다. `batch of N`의 N은 그 `recent[]` 항목의 `merged` 수, 없으면 묶음 줄 수이고 N이 1이면 `batch of` 조각을 뺀다. batch id가 없는 줄은 묶지 않고 머리도 없다 | 개선안 목업. 지금 `+ N more` 규칙(`recentWindow`)을 그대로 두고 그 결과를 묶어 기존 시험과 접기 동작을 지킨다 |
| `board.js`의 최근 머지 줄에 `batch`(문자열 또는 `null`)와 `at`(머지 시각 ms 또는 `null`)을 더하고, 묶음 함수 `recentGroups(rows, recent)`를 내보낸다 | 그리는 쪽은 `recentWindow` 결과를 이 함수에 넘긴다 |
| 패널 폭이 520px 이상이면 펼친 저장소를 두 단으로 그린다. 저장소 머리는 두 단 위 전체 폭. 왼쪽(넓게, 약 1.25:1): 경고·흔들림 줄, 배치, 대기, 반송, 알 수 없는 항목, 빈 구역 한 줄. 오른쪽: 머지 뒤 검사, 막대 줄, Today, Recent. 한쪽이 비면 다른 쪽이 전체 폭이다. Needs you는 저장소들 위 전체 폭 그대로 | 개선안 목업의 `.cols`(`min-width: 520px`에서 `1.25fr 1fr`)가 왼쪽에 지금 도는 것, 오른쪽에 지난 것을 둔다 |
| `board.js`에 `WIDE_WIDTH = 520`과 `isWide(width)`를 더하고, `render`의 `on_size_change`가 `this.narrow`처럼 `this.wide`를 바꾼다(바뀔 때만 다시 그림) | 개선안 "두 단은 `on_size_change` 둘째 기준점". 지금 `NARROW_WIDTH`와 같은 모양이다 |
| 한 단일 때 저장소 안 순서를 머지 뒤 검사 → 막대 줄 → Today → Recent로 한다(지금은 Recent → Today) | 개선안 목업 순서다. 두 단의 오른쪽 순서와 같게 둔다 |
| 막대 줄과 묶음 머리는 다시 그리기를 늘리지 않는다(타이머, `cx.notify()`를 더하지 않는다) | 값은 status를 다시 읽을 때만 바뀐다 |

### 3.4 본체 데이터 (작업 `board-e-data`)

새 필드는 모두 더하기만 하고 `#[serde(default)]`다. 옛 `state.json`, `batch.json`, `history.jsonl`, 옛 허브가 낸 status JSON이 그대로 읽힌다. 있는 필드의 이름과 뜻은 바꾸지 않는다.

| 결정 | 이유 |
|---|---|
| `verify::Options`에 `on_step_end: Option<&dyn Fn(&StepEnded)>`와 `clock: Clock`(기본 `Clock::System`)을 더한다. `StepEnded`는 `{ step, result, duration }`이고 `result`는 `passed`·`failed`·`timed_out`. 걸린 시간은 그 단계의 두 번째 시도(다시 돌림)까지 합친 것이고 `opts.clock.now()`로 잰다. 취소된 단계는 부르지 않는다. 다른 호출자(preflight, post, init setup, flaky probe)는 `..Options::default()`라 바뀌지 않는다 | 제품 코드에 새 `Instant::now(`를 쓰지 않는다(`docs/testing.md`, `product-instant-now` 검사). 멈춘 시계로 시험에서 걸린 시간을 결정적으로 정할 수 있다 |
| 엔진은 `engine.timing.clock`을 넘기고, 끝난 단계를 `BatchRecord.step_times: Vec<StepRun { round, step, seconds, result }>`(`batch.json`)에 쌓는다 | 개선안 "batch 기록에 단계별 시간". 회차(`round`)는 지금 `skipped`와 같은 `verify-<회차>` 번호다 |
| 지금 batch(`state.json`의 `batch`, status의 `batch`)에 `step_started_ms: Option<EpochMs>`, `done: Vec<StepTime { step, seconds }>`(이번 회차에 끝난 단계), `skipped: Vec<String>`(이번 회차에 `paths`로 빠진 단계, 설정 순서), `reruns: Vec<Rerun>`(이 batch의 다시 돌림, `BatchRecord.reruns`와 같은 값)을 더한다. 회차가 시작할 때(`Run::verify`) `done`·`skipped`·`step_started_ms`를 새로 정한다 | status의 `batch`는 `state.batch`를 그대로 낸다. 패널이 지금 회차의 끝난 단계 시간, 지금 단계의 경과, 건너뛴 단계, `Round 2 · rerun` 줄을 그릴 수 있다 |
| `batch_finished` 사건에 `ran: Vec<String>`(첫 검증 회차의 단계 목록), `verifications: u32`, `step_times: Vec<StepTime>`(이 batch에서 통과한 단계 실행)을 더한다 | status는 이미 `history.jsonl`을 읽는다. batch 폴더를 매번 열지 않고 평소 시간을 낸다 |
| status에 최상위 `usual: { batch_s, steps, post_verify_s }`를 더한다. `steps`는 단계 이름마다 최근 통과 실행 20개의 중앙값(초). `batch_s`는 지금 batch가 있을 때, `verifications == 1`이고 `ran`이 지금 batch의 `steps`와 같은 최근 batch 20개의 `seconds` 중앙값. `post_verify_s`는 최근 `post_green` 사건 20개의 `seconds` 중앙값. 표본이 3개 아래면 그 값은 `null`(steps에서는 그 이름이 빠진다). 중앙값은 `today.median_lead_s`와 같은 정수 규칙 | 개선안 "단계 구성이 같은 batch끼리 낸 평소 시간"(문제 1의 제대로 된 해결)과 "머지 뒤 평소 시간". 다시 돌림·나눠 찾기가 낀 batch는 길어서 평소 값에서 뺀다. 표본 수 3은 mod의 `MIN_USUAL_SAMPLES`와 같다 |
| status의 `daemon`에 `doing: Option<String>`을 더한다(`daemon.json`의 값 그대로, 데몬이 돌 때만) | 개선안 "데몬이 지금 하는 일". 본체가 쓰는 한국어 문장이라 mod는 그대로 보인다(경고 문장과 같다) |
| `chute batches --json` 항목에 `verify_s`, `verifications`, `skipped`, `reused`, `reruns`, `step_times`를 더한다(`BatchRecord` 값 그대로) | F의 Batches 탭이 쓸 수 있다. F는 이 필드가 없어도 그린다(F가 먼저 병합될 수 있다) |
| `tests/fixtures/status-full.json`에 새 필드를 모두 넣는다. `status_fixtures_round_trip`이 모든 필드가 있는지 본다 | 저장소 규칙. mod 시험도 이 fixture를 쓴다 |
| `chute status`의 글 출력(사람용)은 바꾸지 않는다 | 요청 범위는 status JSON과 패널이다 |

### 3.5 본체 데이터 그리기 (작업 `board-e-show`)

| 결정 | 이유 |
|---|---|
| 배치의 평소 시간: status에 `usual`이 객체로 있으면 `usual.batch_s`를 쓴다(`null`이면 평소 시간 없음). `usual`이 없으면(옛 chute, 옛 허브) 지금 `usualSeconds(recent)`(60초 이상 표본)를 쓴다 | 문제 1을 끝낸다. 같은 구성의 표본이 모자랄 때 다른 구성의 값을 보이면 다시 틀린 기준이 된다. 옛 본체에서는 B의 동작을 지킨다 |
| 단계 줄: 끝난 단계 `✓ check 48s · clippy 1m 11s`(시간은 `batch.done`에 있을 때만), 지금 단계 `▸ server-test  4 of 5 · 2m 21s / 4m 50s`(경과는 `step_started_ms`, 평소는 `usual.steps`에 있을 때만), 건너뛴 단계 `– account-server-test · setup-test · skipped (paths)`(`batch.skipped`가 비지 않을 때) | 개선안 안 2의 점선 값들 |
| 단계 막대에 건너뛴 단계를 칸으로 더한다: 실행 단계 칸들 뒤에 옅은 단색(빗금 대신) 칸, tooltip `name · skipped (paths)` | 개선안 "빗금은 `linear_gradient`가 반복을 못 하면 옅은 단색으로 대신". 실행 단계와 건너뛴 단계의 설정 안 순서는 status에 없어 뒤에 모은다 |
| 국면 줄에 다시 돌림: `batch.reruns`의 마지막 항목이 있으면 `rerun: known flaky`(신호 `known_flaky`) 또는 `rerun: different tests`(`different_tests`)를 `Round N` 뒤에 더한다 | 개선안 "Round 2 · rerun: known flaky" |
| 머지 뒤 검사: `usual.post_verify_s`가 있으면 도는 줄 오른쪽이 `5m 17s / usually 7m 7s`이고 막대는 경과/평소 비율(1에서 멈춤)만큼 채운다. 넘으면 `ansi3`. 평소 값이 없거나 슬롯 대기면 C의 막대(전체 폭, 깜빡임) 그대로 | C가 E 전까지 미룬 평소 시간 |
| 데몬 점 tooltip에 `daemon.doing`을 둘째 줄로 더한다 | 개선안 "저장소 점 tooltip" |
| 새로 다시 그리기를 늘리지 않는다. 경과 시간은 지금처럼 `tick`이 다시 그린다 | 배치 경과와 같은 규칙이다 |

### 3.6 상세 창 (작업 `board-f-detail`)

| 결정 | 이유 |
|---|---|
| 창은 보고서 창과 같은 `openOverlay({ kind: "window", width: 760, height: 520 })`이고 key는 `chute-detail` 하나다(다시 열면 바꾼다). props에 `{ detail: { repo, name, tab }, bin }`을 넘기고, 창 안의 같은 mod가 `init`에서 `detail`을 보고 상세 화면을 그린다 | 개선안 "보고서 창과 같은 방식의 창". 지금 `file` props 갈래와 같은 구조다 |
| 입구 둘: D의 `Last batches` 제목 줄 오른쪽 `Details ›`(Overview 탭), A의 만성 흔들림 줄 오른쪽 `Ledger ›`(Flaky 탭). 흔들림 줄을 눌러 펼치는 지금 동작은 그대로 둔다 | 개선안 목업의 두 입구 |
| 창이 열릴 때 세 명령을 함께 부른다: `chute --repo <path> batches --json -n 120`, `chute --repo <path> flaky --json`, `chute --repo <path> events --since 1d --limit 2000 --json`. 탭마다 읽는 중·오류·빈 상태를 따로 보인다. 머리의 `Refresh`가 셋을 다시 부른다. 주기적으로 다시 읽지 않는다 | 개선안 "열 때만 부른다". `events` 기본 상한 500은 하루치(727건)보다 작다. 원격 허브는 명령 셋의 지연이 있으니 탭마다 따로 채운다. 명령은 `--repo`로 저장소를 정하고 허브가 다른 PC면 명령이 스스로 넘긴다(`run_on_hub`) |
| `events` 출력이 상한에 걸렸다는 경고(stderr)가 있으면 Events 탭 위에 `Showing the latest 2000 events` | 일부만 보이는 것을 숨기지 않는다 |
| Overview: 숫자 칸 넷(batch 수, 머지된 PR 수, 반송된 PR 수, 1분 이상 batch의 걸린 시간 중앙값) → 막대 타임라인(batch 하나가 막대 하나, 옛것이 왼쪽, 높이는 걸린 시간 로그 눈금, 반송 있는 batch `ansi1`, 되돌린 batch는 흐리게, tooltip에 시각·걸린 시간·PR) → 막대 아래 처음·가운데·끝 시각 → 한 줄 `Bounce reasons in 24h: 10 conflict, 6 verify`(events의 `bounced` 사건을 `reason`으로 센다) | 개선안 상세 창 목업. 막대를 시각 위치에 놓으려면 절대 위치가 필요해 순서대로 같은 폭으로 둔다 |
| Flaky: 장부 표(시험, 단계, 횟수, 마지막 시각 `formatAgo`, 만성 표시). 만성 먼저, 그다음 횟수 많은 순. 20줄과 `+ N more` 펼침 | 개선안 Flaky 탭. 장부가 22건처럼 길 수 있다 |
| Events: 새것부터 `HH:MM  기호  문장`. 사건 종류마다 영어 문장(예: `merged #616`, `bounced #616 · conflict · attempt 1`, `flaky e2e/x.spec.ts:160`, `post-merge green c21b6f2`, `post-merge failed e2e · culprit #539`, `queue stopped · base broken c05a894`, `resumed by repair #588`). 모르는 종류는 종류 이름 그대로 | 개선안 Events 목업. 본체가 사건 종류를 더해도 깨지지 않는다 |
| Batches: 새것부터 한 줄에 batch id 짧게, 시작 시각, 걸린 시간, 머지 `#a #b`, 반송 `#c`, 되돌림 칩. 항목에 `step_times`·`skipped`·`reruns`가 있으면(E 뒤) 둘째 줄에 단계 시간 요약 | E가 먼저 병합되든 뒤에 병합되든 그린다 |
| `detail.js`는 gpui를 들여오지 않는 순수 함수만 둔다(명령 인자, JSON·NDJSON 해석, 타임라인 막대, 반송 이유 집계, 사건 문장, 장부 정렬). 창 그리기는 `main.js`에 둔다. 시험은 `detail.test.js` | `board.js`/`main.js`와 같은 나눔이다. `node --test`가 gpui 없이 돈다 |
| 막대나 줄을 눌러 batch 로그를 여는 동작은 넣지 않는다 | `batches --json`과 `flaky --json`에는 어느 회차의 어느 로그인지가 없다. 경로를 짐작하면 틀린 로그를 연다. 5절에 적는다 |

## 4. 작업

| 작업 | 맡는 것 | 순서 | 방식 | 방식의 이유 |
|---|---|---|---|---|
| `board-d-time` | 3.3 | 처음(다른 플랜과 병렬) | `basic` | mod 화면 조정과 순수 데이터 함수 |
| `board-e-data` | 3.4 | `retry-fix`, `report-fix` 뒤 | `poteto` | 상태 저장 형식(`state.json`, `batch.json`, `history.jsonl`)에 필드가 늘고 검증 실행부를 고친다 |
| `board-e-show` | 3.5 | `board-e-data`, `board-d-time` 뒤 | `basic` | status에서 받는 값을 보이는 화면 조정 |
| `board-f-detail` | 3.6 | `board-d-time` 뒤, `board-e-show`와 병렬 | `poteto` | 새 창과 새 모듈, 설치 목록 변경에 걸친 새 설계 |

순차로 둔 이유는 3.1에 적었다.

## 5. 범위 밖

- 상세 창의 7일 추세 막대(일별 머지 수, 중앙값). 개선안 ④가 "chute에 추가 필요"라 했고 요청의 E 목록에 없다.
- 상세 창에서 batch 로그나 흔들린 시험의 마지막 로그 열기(3.6 마지막 결정). 열려면 본체가 batch마다 회차별 로그 경로를 내야 한다.
- Events 탭의 PR 번호 필터와 검색.
- 모든 저장소를 합친 막대 줄(개선안 목업의 `LAST 24H · ALL REPOS`). D의 막대 줄은 저장소마다다.
- e2e 시험별 시간(개선안 ④ 마지막 줄, chute 밖 산출물).
- `chute status`의 사람용 글 출력, 패널 탭 배지와 toast 규칙, `mod/chute/mod.json`의 패널 크기, `mod/chute-tabs/`.

## 6. 사용자가 정해야 하는 결정

없다. 검증 단계를 빼거나 시간 제한을 늘리거나 `paths`를 줄이는 작업은 없다. `node --test mod/*/*.test.js`와 cargo 시험은 `chute.toml`의 검사라 chute가 병합 전에 돌리고, 지시서의 성공 조건에는 작업마다 더하거나 고치는 시험을 이름으로 적었다.

플랜이 스스로 정해 사용자가 다르게 원할 수 있는 것(시작 전에 알려 주면 그 작업의 지시서를 고친다):

1. E를 본체(`board-e-data`)와 화면(`board-e-show`) 두 PR로 나눴다. `board-e-data`가 `report-fix` 뒤라 E의 화면은 D·F보다 늦게 나온다.
2. 같은 단계 구성의 표본이 3개 아래면 배치의 평소 시간을 보이지 않는다(3.5 첫 결정). 다른 구성의 값으로 채우기를 원하면 바꿀 수 있다.
3. 한 단 배치에서 저장소 안 순서를 Today → Recent로 바꾼다(3.3). 지금 순서를 원하면 그대로 둘 수 있다.
