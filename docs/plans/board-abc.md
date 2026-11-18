# 현황판 개선안 2 플랜: 할 일 묶음(A), 단계 막대(B), 반송과 머지 뒤 검사(C)

> 작성일 2026-11-19. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

board-ux2 조사가 현황판 개선안 2를 artifact(https://claude.ai/artifact/5GiHNDPsr4UfbZomupSMbC, 사본 `/home/dev/.claude/orch/reports/board-ux2/chute-board-ux2.html`)로 보였고, 사용자는 페이지의 "추천대로 진행"을 골랐다. 이번 플랜은 그 가운데 묶음 A·B·C를 구현한다. 기준은 main `2b7d38f`이다(`mod/chute/main.js` 1226줄, `board.js` 914줄).

개선안이 짚은 지금 화면의 문제 가운데 이 플랜이 푸는 것:

| 번호 | 문제 | 근거 | 묶음 |
|---|---|---|---|
| 1 | 배치의 `usually`가 단계를 건너뛴 짧은 batch까지 넣은 중앙값이라 정상 batch가 "Slower than usual"로 보인다 | `board.js` `usualSeconds` | B(줄임) |
| 2 | 만성 흔들림 경고가 시험마다 카드 하나라 맨 위를 차지한다 | `board.js` `alertsOf`의 warnings 반복 | A |
| 3 | 같은 수가 네 칸 숫자, 구역 제목, 머리줄에 세 번 나온다 | `main.js` `cellsView`, `sectionTitle`, `summaryView` | A |
| 4 | 단계 막대가 국면 네 칸이고 실제 단계(`batch.steps`)는 글자뿐이다 | `main.js` `batchView`의 `STAGE_NAMES` | B |
| 5 | 반송 줄에 이유(`last_failure.summary`)가 없고 재시도는 `Retries 2/3` 글자뿐이다 | `main.js` `bouncedRow` | C |
| 6 | 머지 뒤 검사는 경과 시간 한 줄뿐이다 | `main.js` `postVerifyView` | C(일부) |
| 7 | 300px에서 저장소 머리의 상태 문장이 잘린다 | `main.js` `repoHead` | B |

할 일(승인 대기, 반송)은 저장소 안 구역에 묻혀 있어, 저장소가 둘 이상이면 내려가며 찾아야 한다. A가 이것을 저장소 위로 올린다.

## 2. 확정된 결정

요청(사용자가 고른 페이지 ⑥ 권장안)에 적힌 것이다.

| 번호 | 결정 |
|---|---|
| Q1 | 여러 저장소 첫 화면은 안 A: 저장소를 가로지르는 할 일 묶음을 맨 위에 두고 그 아래 저장소를 지금처럼 쌓는다 |
| Q2 | 네 칸 숫자(Batch · Queued · Bounced · Merged today)를 뺀다 |
| Q3 | Needs you에 올릴 것: 승인 대기, 멈춘 큐, 머지 뒤 검사 실패, 재시도가 한 번 남은 반송, 작업자 터미널이 죽은 반송. 만성 흔들림은 한 줄로 묶기만 한다 |
| Q4 | 배치 카드는 안 2(단계 막대). 본체 작업 E는 하지 않는다. status에 있는 단계 목록과 지금 단계만으로 그리고, 단계별 시간·건너뛴 단계·단계별 평소 시간은 넣지 않는다 |
| Q5 | 상세 창(F)은 하지 않는다 |
| Q6 | 이번 묶음은 A, B, C. D·E·F는 써 본 뒤 정한다 |
| — | 화면 문구는 영어(`AGENTS.md`), gpui-kit 제약을 지킨다 |
| — | board-redesign·board-narrow 동작(좁은 폭 배치 등)을 깨지 않는다. `node --test mod/*/*.test.js`가 통과한다 |
| — | 묶음마다 격리 창 스크린샷(`xvfb-run`과 임시 `HERDR_APP_HOME`, 기존 board 작업과 같은 방식)을 paper/graphite, 300·360px로 찍는다 |
| — | 순서는 플래너가 정한다(페이지 권장: A → B·C) |

## 3. 스스로 정한 결정과 이유

### 3.1 공통

| 결정 | 이유 |
|---|---|
| 작업 셋: `board-a-todo`(A), `board-b-steps`(B), `board-c-bounce`(C). A를 먼저, B와 C는 A 뒤에 병렬 | 셋 다 `main.js`, `board.js`, `board.test.js`를 고친다. A는 `repoHead`, `bouncedRow`, `repoView`를 다시 짜고 B는 `repoHead`, C는 `bouncedRow`를 다시 짜므로 B·C는 A 뒤다(같은 함수를 다시 짠다). B와 C는 서로 다른 함수(B: `batchView`, `repoHead`, `usualSeconds`, `batchOf` / C: `bouncedRow`, `postVerifyView`, `cardOf`)를 고쳐 병렬로 둔다 |
| B와 C는 세 파일을 `shared`로 적고, 지시서에 새 시험을 파일 끝이 아니라 관련 시험 옆에 넣고 `import` 목록과 서로의 함수를 건드리지 말라고 적는다 | 두 PR이 파일 끝에 시험을 더하면 같은 자리라 git 충돌이 난다. 서로 다른 자리에 넣으면 chute가 함께 머지할 수 있다 |
| `interpret`의 `sections` 모양은 바꾸지 않고 필드를 더한다. 저장소 안에 그리지 않게 된 구역(`human`)도 `sections`에는 남긴다 | `board.test.js`의 많은 시험이 `sections`로 구역 배치를 본다. 데이터 모양을 지키면 기존 시험이 그대로 뜻을 지킨다. 그리는 쪽(`main.js`)이 무엇을 어디에 그릴지 고른다 |
| 시험 `random malformed status never breaks the board`의 `CONTAINERS`(객체를 담아도 되는 키 목록)에 세 작업이 더할 키(`todo`, `item`, `needs`, `flaky`, `messages`, `stepBar`, `retryDots`)를 A가 한 번에 더하고 `cells`를 뺀다. B·C는 그 줄을 고치지 않는다 | 그 시험은 결과의 모든 객체가 허용된 키 아래에 있는지 본다. B와 C가 같은 줄을 고치면 병렬 PR이 충돌한다 |
| 기존 시험의 기대가 새 뜻 때문에 바뀌면 그 시험의 기대값이나 그 시험 안의 입력만 고친다. 단언을 빼지 않는다. 고친 시험 이름과 이유를 보고서에 적는다. 지운 기능의 시험만 지울 수 있고(지시서에 이름을 적었다), 그것도 보고서에 적는다 | 뜻이 바뀐 곳을 리뷰가 시험 diff로 볼 수 있게 한다 |
| 화면 확인은 `docs/plans/board-redesign.md` 4절과 board-narrow가 실제로 쓴 방법(`/home/dev/.claude/orch/reports/board-narrow/board-narrow.md`의 "화면 확인")으로 한다. 작업마다 장면을 정해 paper/graphite × 300/360px 네 장을 찍는다 | 요청의 결정이다. 300px 패널의 루트 너비가 300, 360px가 360으로 재졌다(board-narrow) |
| 새 파일을 mod에 더하지 않는다 | `chute install`은 `main.js`, `board.js`, `mod.json`만 넣는다(board-redesign 3.1) |

### 3.2 할 일 묶음 (작업 `board-a-todo`)

#### Needs you에 무엇이 올라가나

| 결정 | 이유 |
|---|---|
| Needs you 항목(할 일)은 다음 일곱 가지다. (1) 멈춘 큐(`blocked` 경고) (2) 머지 뒤 검사 실패(`post_verify` 경고) (3) 꺼진 데몬(`daemon` 경고, 큐가 있을 때만) (4) 닿지 않는 허브 (5) 승인 대기 (6) 작업자 터미널이 죽은 반송 (7) 재시도가 한 번 남은 반송 | (1)(2)(5)(6)(7)은 사용자가 고른 것이다. (3)(4)는 사용자가 고른 목록에 없지만 지금 `overview().issues`가 세는 것이다(오류 경고와 닿지 않는 허브). 넣지 않으면 할 일 수가 지금 issues보다 줄어 탭 버튼이 문제를 놓친다. 그래서 규칙을 "지금 issues 전부 + 반송 두 종류"로 둔다 |
| "재시도가 한 번 남은"은 `attempts >= budget`인 반송이다. 남은 재시도 수 `left = budget - attempts + 1`(1 아래로 내려가지 않음)로 센다. `attempts`나 `budget`이 0 이상의 정수가 아니거나 `budget`이 0이면 `left`는 `null`이고 올리지 않는다 | 본체는 실패를 기록할 때 `attempts >= budget`이면 큐에서 뺀다(`src/queue.rs` `record_failure`, `src/engine.rs` `bounce`). `attempts == budget`인 반송은 다시 넣으면 한 번 더 돌고, 그것도 실패하면 빠진다. 즉 재시도 하나가 남았다 |
| "작업자 터미널이 죽은"은 `agent`가 없거나 `agent.alive !== true`인 반송이다 | 반송을 고칠 터미널이 없다는 점에서 같다. 지금 반송 줄의 `No agent`와 `· exited`가 이 둘이다 |
| 순서: 오류(멈춘 큐, 머지 뒤 검사 실패, 꺼진 데몬, 닿지 않는 허브) → 승인 대기 → 작업자가 죽은 반송 → 재시도 하나 남은 반송. 같은 무리 안에서는 저장소 정렬 순서(지금 `gradeOf`), 그 안에서는 지금 구역 안 순서(제출 순) | 개선안: "작업자 터미널이 죽었으면 더 위". 오류는 큐 전체를 막으므로 먼저다. 둘 다 해당하는 반송은 작업자가 죽은 무리에 한 번만 둔다 |
| 만성 흔들림(`flaky_chronic`)은 Needs you에 올리지 않고 할 일 수에도 넣지 않는다 | 사용자 결정 Q3 |
| `board.js`: `interpret`의 보드 결과에 `todo` 배열을 더한다. 항목은 `{ key, kind, repoId, repoName, tone, item }`. `kind`는 `blocked`, `post_verify`, `daemon`, `hub`, `approval`, `bounce` 가운데 하나. `item`은 그 저장소 `sections`에 이미 있는 경고나 카드 객체를 그대로 가리킨다(`hub`는 `{ text: "Hub unreachable", detail: hub.error }`) | 그리는 쪽이 기존 `alertCard`·`humanCard`·`bouncedRow`를 그대로 쓴다. 데이터가 한 곳에서 나와 할 일 수와 목록이 어긋나지 않는다 |
| 반송 카드에 `left`(위 정의)와 `needs`(올린 이유 목록: `{ kind: "worker_gone", text: "Worker exited" }`, `{ kind: "last_retry", text: "1 retry left" }`)를 더한다. `needs`가 비지 않은 반송만 할 일이다 | 반송 줄과 Needs you가 같은 이유 칩을 보인다. C의 재시도 점도 `left`를 쓴다 |

#### 숫자가 뜻하는 것

| 결정 | 이유 |
|---|---|
| `overview(board).issues`는 `board.todo.length`다. 공유 값 `chute.status`의 필드 이름(`grade`, `issues`, `running`, `repos`)은 그대로다 | 할 일 묶음이 지금 issues의 모든 것을 담으므로(위 첫 결정) 뜻이 "할 일 수"로 넓어질 뿐 줄지 않는다. 필드 이름을 지키면 탭 버튼 mod가 그대로 읽는다 |
| 상태 줄(`statusLineOf`)의 `Needs you · N`은 승인 대기와 할 일인 반송을 함께 센다. 오류 줄이 먼저인 순서는 그대로다 | 할 일 반송이 있는 저장소가 `human` tone이 되어 `grade`가 `attention`이 되고, 탭 버튼 점 색이 할 일 배지와 맞는다 |
| 패널 머리: `chute` 다음에 할 일이 있으면 `N to do` 칩(색: grade가 `failed`면 `ansi1`, 아니면 `blocked`), 그 뒤에 흐린 `N repos · N running`(실행 중이 0이고 할 일도 0이면 `N repos · all clear`), 오른쪽에 새로 읽은 시각. 좁을 때(`this.narrow`)는 `repos · running` 글자를 빼고 칩과 시각만 둔다. `overviewText`의 셋째 값은 `N to do`가 된다 | 개선안 목업. `0 issues` 글자가 할 일 배지로 바뀐다. 300px 목업은 칩과 시각만 둔다 |
| 탭 버튼 mod(`mod/chute-tabs/chute.js`)의 `chuteTip`은 `N issues` 대신 `N to do`를 쓴다 | 패널 머리와 탭 버튼이 같은 말을 한다. 공유 값의 뜻이 바뀌는 쪽과 같은 PR에서 맞춘다(요청의 위험 항목) |
| 패널 탭 배지(`badgeText`, `setBadge`)와 알림(`noticesOf`, toast)은 바꾸지 않는다 | 알림은 새로 생긴 일만 알리는 다른 규칙이다. 반송마다 toast를 내면 시끄럽다. 배지 수와 할 일 수가 다를 수 있는 것은 지금(issues와 배지)도 같다. 5절에 적는다 |

#### 그리는 곳

| 결정 | 이유 |
|---|---|
| 본문 맨 위(저장소들 위)에 `NEEDS YOU  N` 구역을 둔다. 할 일이 없으면 그리지 않는다. 경고는 `alertCard`, 승인은 `humanCard`, 반송은 `bouncedRow`로 그리고, 저장소가 둘 이상이면 항목마다 저장소 이름을 작게(11px, `dim`) 붙인다 | 개선안 안 A. 저장소가 하나면 이름이 군더더기다(300px 목업) |
| 할 일로 올라간 경고(`blocked`, `post_verify`, `daemon`)와 승인 카드는 저장소 안에 다시 그리지 않는다. 저장소 Bounced 구역의 할 일 반송은 그 자리에 흐린 한 줄 `#608 is under Needs you`로 남기고, 구역 제목 수는 지금처럼 반송 전체 수다 | 같은 카드가 두 번 나오지 않게 한다. 반송 수를 지키고 어디로 갔는지 알린다(개선안 목업) |
| 저장소가 접혀 있어도 Needs you는 보인다. 자동 펼침(`nextFold`, `problems`)은 바꾸지 않는다 | 할 일이 접힘과 상관없이 보이므로 펼침 규칙을 바꿀 까닭이 없다. 바꾸면 사용자가 저장해 둔 접힘이 다르게 움직인다 |
| 만성 흔들림 경고는 저장소 안 경고 자리에 한 줄로 묶는다: `△ N chronic flaky test(s)`, 오른쪽에 가장 늦은 `at_ms`의 `formatAgo`. 누르면 경고 문장들이 그 아래 펼쳐진다(펼침은 메모리에만, 저장하지 않는다). 다른 경고(`not_merged`, `config` 등)는 지금처럼 카드다. `board.js`는 저장소에 `flaky: { count, ago, messages } | null`을 더하고, `alerts` 구역의 데이터는 바꾸지 않는다 | 사용자 결정 Q3 "한 줄로 묶기만". 목업의 `· e2e`(단계 이름)와 `Ledger ›`는 뺀다. status의 `Warning`은 `kind`, `message`, `pr`, `at_ms`뿐이라 단계 이름은 한국어 문장을 쪼개야 나오고, 장부는 상세 창(F)이다 |
| 네 칸 숫자를 지운다: `main.js`의 `cellsView`, `board.js`의 `cellsOf`와 `cells` 필드, 시험 `count cells are batch, queued, bounced and merged today`를 지우고, `a repo whose hub is unreachable has no count cells and an unknown daemon`은 `cells` 단언만 뺀다 | 사용자 결정 Q2. 쓰지 않는 필드를 남기지 않는다. 지운 기능의 시험이라 검증을 약하게 하는 것이 아니다 |
| 조용한 저장소: 허브에 닿고, 할 일·경고(흔들림 포함)·배치·대기·반송·알 수 없는 항목이 없고, 머지 뒤 검사가 돌지 않는 저장소다(`repo.quiet`). 조용한 저장소는 머리 한 줄만 그린다(접힌 모양 `▸`). 머리를 누르면 이번 세션 동안만 펼친다(메모리, 저장하지 않음). 머리의 `Collapse all/Expand all`은 조용한 저장소의 펼침도 함께 닫고 연다 | 개선안 "조용한 저장소 한 줄". 사용자가 저장한 접힘(`chute.fold.<id>`)과 섞지 않으려고 따로 둔다. 일이 생기면 조용하지 않게 되어 저절로 지금처럼 그려진다 |
| 접힌 저장소와 조용한 저장소의 머리 상태 글자는 요약 `repo.summary`다. 상태 tone이 `ok`이면 `All clear · N merged today · #PR <ago>`(`today.merged`가 정수일 때와 최근 머지가 있을 때만 그 조각), 아니면 상태 글자 그대로. 펼친 저장소는 지금처럼 상태 글자다 | 개선안 "접힌 저장소는 한 줄에 요점을 싣는다" |
| 빈 구역 한 줄(`Empty: …`)에서 `Needs you`를 뺀다. 조용한 저장소는 펼쳐도 이 줄을 그리지 않는다 | Needs you는 저장소 밖으로 갔다. 조용한 저장소는 모든 구역이 비었으니 그 줄이 새 정보를 주지 않는다 |

### 3.3 단계 막대 (작업 `board-b-steps`)

| 결정 | 이유 |
|---|---|
| `usualSeconds`는 `seconds >= 60`인 batch만 표본으로 쓴다(`MIN_USUAL_SECONDS = 60`, 내보냄). 표본이 3개(`MIN_USUAL_SAMPLES`) 아래면 지금처럼 `null` | 요청의 임시 기준(문제 1). 단계를 건너뛴 짧은 batch를 뺀다. 제대로 된 해결(같은 단계 구성끼리의 중앙값)은 E다 |
| `batchOf`가 `stepBar`를 더한다: `steps`의 이름마다 `{ name, state }`, `state`는 `done`·`current`·`todo`. 국면이 `pushing`이면 모두 `done`, `building`이면 모두 `todo`, 그 밖(verifying, bisecting, 모르는 값)은 지금 단계(`step`)의 순번 앞이 `done`, 그 자리가 `current`, 뒤가 `todo`. 지금 단계가 목록에 없으면 모두 `todo`. 단계 목록이 비면 `stepBar`는 빈 배열 | status의 `batch.steps`와 `batch.step`만으로 된다(Q4). bisecting은 같은 단계들을 다시 돌리므로 verifying과 같이 그린다 |
| 배치 카드: 머리(돌기 표시, `CURRENT BATCH`, 경과 `/ usually`)와 PR 줄은 그대로. 그 아래 단계마다 한 칸인 4px 막대(`done`은 `ansi2`, `current`는 지금 채움 색 규칙 그대로, `todo`는 `border`), 그 아래 글줄 셋: 끝난 단계 `✓ a · b · c`(`dim`, 줄바꿈), 지금 단계 `▸ name  n of m`(`working`, 오른쪽에 지금처럼 `Slower than usual`), 남은 단계 `○ d · e`(`dim`). 빈 줄은 그리지 않는다. 맨 아래 국면 줄 `Verifying · Round 1 · Reused preflight pass`(11px, `dim`). 슬롯을 기다리면 지금 단계 줄이 `Waiting for a slot`(`muted`)이다. 단계 목록이 없으면 막대와 단계 줄 없이 지금 단계 글자(`stepText`, 없으면 국면)만 | 개선안 안 2에서 status로 되는 부분. 끝난 단계를 한 줄로 접은 것은 패널 목업이다. 국면 네 칸(Building·Bisecting·Pushing)은 국면 줄 문장이 된다 |
| 국면 네 칸 막대를 지운다: `main.js`의 `STAGE_NAMES` 사용, `board.js`의 `STAGE_NAMES`·`stageOf`·`stage` 필드, 시험 `the batch stage is the index of the phase, and null for a missing or unknown phase` | 안 2가 대신한다. 쓰지 않는 값을 남기지 않는다 |
| 건너뛴 단계의 빗금은 그리지 않는다 | 건너뛴 단계는 status에 없다(E). 요청의 "빗금은 linear_gradient가 반복을 못 하면 옅은 단색"은 그릴 데이터가 생길 때의 규칙이다 |
| 좁을 때(`this.narrow`) 저장소 머리의 상태 글자를 이름 아래 둘째 줄로 내린다. 둘째 줄은 이름 칸에 맞춰 들여 쓰고 줄바꿈한다(잘라 내지 않음). 넓을 때는 지금처럼 오른쪽 끝 | 문제 7. 300px에서 `Verifying · te…`로 잘린다 |

### 3.4 반송과 머지 뒤 검사 (작업 `board-c-bounce`)

| 결정 | 이유 |
|---|---|
| 반송 카드에 `summaryLine`을 더한다: `last_failure.summary`의 첫 줄(`firstLine`), 비면 `null`. 반송 줄은 칩 줄 아래에 이것을 12px `muted` 한 줄로 그리고(넓을 때 잘라 내고 tooltip에 원문 전체, 좁을 때도 같다) | 문제 5. 요약 문장은 본체가 준 데이터라 영어 규칙 밖이다(승인 카드도 그대로 보인다) |
| 재시도 점: 반송 카드에 `retryDots: { total, used, tone, tip } | null`. `budget`이 1~10의 정수이고 `left`(A)가 있을 때만 만든다. `total = budget`, `used = min(attempts, budget)`, `tone`은 `left === 1`이면 `warn` 아니면 `dim`, `tip`은 `Bounced A of B · N retries left`(1이면 `retry`). 줄에서는 `Retries 2/3` 글자 자리에 7px 점 `total`개(채운 점 `used`개는 tone 색, 나머지는 `border`)를 그린다. `retryDots`가 `null`이면 지금 글자를 그대로 둔다 | 개선안 "재시도는 점 세 개". 한도를 넘는 점 개수는 좁은 폭을 깨므로 10에서 자른다 |
| 머지 뒤 검사가 도는 동안 그 줄 아래 4px 막대를 줄 폭 전체로 그린다. 색은 `working`, 슬롯을 기다리면 `muted`. 돌 때만 `loop("opacity", …)`로 깜빡인다(저장소 점과 같은 값). 평소 시간과 진행 비율은 넣지 않는다 | 요청: 평소 시간은 E 전까지 생략. status의 `post_verify.run`에는 `started_ms`와 `step`뿐이라 비율을 낼 수 없다. 깜빡임은 네이티브라 다시 그리지 않는다 |
| 검사만 도는 동안 저장소 상태 줄이 `All clear`인 것(문제 6 뒷부분)은 고치지 않는다 | 요청의 C 내용에 없다. 고치면 `grade`와 탭 버튼의 running 수가 바뀐다. 5절에 적는다 |

## 4. 작업

| 작업 | 맡는 것 | 순서 | 방식 | 방식의 이유 |
|---|---|---|---|---|
| `board-a-todo` | 3.2 | 처음 | `poteto` | 저장소를 가로지르는 새 구조(`todo`)와, 다른 mod가 읽는 공유 값(`chute.status`)의 뜻을 바꾸고 두 mod에 걸친다 |
| `board-b-steps` | 3.3 | `board-a-todo` 뒤, C와 병렬 | `basic` | 화면 조정과 순수 함수 하나의 기준 변경이다 |
| `board-c-bounce` | 3.4 | `board-a-todo` 뒤, B와 병렬 | `basic` | 화면 조정이고 status에서 이미 받는 값을 보일 뿐이다 |

B·C가 A 뒤인 이유: A가 `repoHead`(요약 글자), `bouncedRow`(이유 칩, 저장소 이름), `repoView`(그릴 곳)를 다시 짜고, B는 `repoHead`, C는 `bouncedRow`를 다시 짠다. C의 재시도 점은 A의 `left`를 쓴다. B와 C는 고치는 함수가 겹치지 않아 병렬이다(3.1).

## 5. 범위 밖

- 묶음 D(시간축), E(본체 데이터: 단계별 시간, 건너뛴 단계, 재검증, 머지 뒤 평소 시간), F(상세 창). 사용자 결정 Q5·Q6.
- chute 본체(`src/`), status JSON, `tests/fixtures/`, `chute.toml`, `mod/chute/mod.json`(패널 크기 360).
- 패널 탭 배지(`badgeText`)와 toast 알림(`noticesOf`)의 규칙. 배지 수가 할 일 수와 다를 수 있다(배지는 멈춘 큐, 머지 뒤 실패, 승인 대기만 센다).
- 머지 뒤 검사만 도는 동안의 저장소 상태 줄(문제 6 뒷부분), 흔들림 묶음 줄의 단계 이름과 장부 링크.
- 자동 펼침 규칙(`nextFold`, `problems`).

## 6. 사용자가 정해야 하는 결정

없다. 검증을 약하게 하는 작업은 없다. 지우는 시험은 지우는 기능(네 칸 숫자, 국면 막대)의 시험뿐이다. `node --test mod/*/*.test.js`는 `chute.toml`의 검사라 chute가 병합 전에 돌리고, 지시서의 성공 조건에는 작업마다 더하거나 고치는 시험을 이름으로 적었다.

사용자가 고른 Needs you 목록에 꺼진 데몬과 닿지 않는 허브를 더한 것(3.2 첫 결정)은 지금 issues의 뜻을 줄이지 않으려는 플래너의 결정이다. 빼기를 원하면 A 시작 전에 알려 주면 된다.
