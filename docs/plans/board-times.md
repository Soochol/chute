# 현황판 항목별 시간과 빈 구역 머리 유지 플랜

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 현황판 mod(`mod/chute/`)를 보고 "걸린 시간을 각 항목별로 보여주게 하는건? 그리고 헤더는 유지시키기? 개체가 없어도? 헤더 - 항목 (걸린시간) 이렇게?"라고 했고, 오케스트레이터의 검토와 추천에 "좋아"라고 답했다.

지금 현황판(main `44db676`, board-polish 머지 뒤 기준)은 이렇다.

- 시간은 CURRENT BATCH의 경과, POST-MERGE의 경과와 `32m ago`, RECENT의 머지 시각(`41m ago`)에만 있다. QUEUED, NEEDS YOU, BOUNCED 행에는 시간이 없다.
- RECENT는 언제 머지됐는지만 보이고, PR이 큐에 들어와 머지되기까지 얼마나 걸렸는지는 보이지 않는다.
- 구역은 항목이 있을 때만 그려진다(`board.js` `repoBoard`의 `SECTION_ORDER.filter((id) => placed[id].length > 0)`). 큐가 비면 구역 머리가 모두 사라져 화면의 모양이 매번 바뀐다.

status JSON(`chute status --json`)에서 확인한 사실:

- `queue[]`에는 `submitted_ms`가 있다. 처음 submit한 시각이고, 큐에 남아 있는 동안 다시 submit해도 바뀌지 않는다(`src/queue.rs` `submit`, 시험 `a_bounced_pr_goes_back_to_waiting_keeping_its_count_and_place`). `chute drop` 뒤 다시 submit하면 새 항목이라 새 시각이다.
- `recent_merged[]`는 `pr, title, batch, merged_ms, bounces`뿐이다. 그런데 그 출처인 `history.jsonl`의 `merged` 이벤트에는 `submitted_ms`가 **이미 있다**(`src/model.rs` `EventKind::Merged`, 필수 필드). `today.median_lead_s`가 이 값으로 중앙값을 낸다. 요청은 "본체가 머지 기록에 submit 시각을 남긴다"고 했지만 기록은 이미 남고 있어서, 본체는 `status`의 보기에 그 값을 옮기기만 하면 된다. history와 `state.json` 형식은 바뀌지 않는다.
- `queue[].last_failure`(`FailureView`)에는 시각이 없다. 상태 파일의 `FailureRecord.at_ms`를 보기로 옮길 때 빠진다. 게다가 HEAD가 바뀌어 반송된 PR(`state_reason: head_changed`)은 `last_failure`를 새로 쓰지 않는다(`queue::head_changed`). 앞선 반송의 실패가 남아 있으면 그 시각은 이번 반송의 시각이 아니다. 반면 반송은 모두 `history.jsonl`에 `bounced` 이벤트로 남는다(`src/engine.rs`의 `head_changed`와 `bounce`). 반송 시각은 이 이벤트에서 가져온다.

## 2. 확정된 결정

요청에 적힌 것이다.

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | 시간의 자리 | 항목 행의 오른쪽 끝(아이콘 `log`·`↗` 바로 왼쪽) |
| 1a | QUEUED | 기다린 시간 `waiting 3m`. 기준은 `submitted_ms` |
| 1b | NEEDS YOU | 기다린 시간. 기준은 `submitted_ms` |
| 1c | BOUNCED | 반송된 지 얼마 `12m ago` |
| 1d | CURRENT BATCH, POST-MERGE | 지금처럼 경과 |
| 1e | RECENT | submit부터 머지까지 걸린 시간만 보인다(안 B). 머지 시각(`41m ago`)은 hover tooltip |
| 2 | 본체 | `recent_merged`에 submit 시각을 더한다. 더하기만 하고 값이 없으면 mod는 시간 자리를 비운다 |
| 2a | 어느 submit부터 | 처음 submit부터. 반송 횟수는 이미 `bounces`로 보인다 |
| 3 | 빈 구역 머리 | NEEDS YOU, CURRENT BATCH, QUEUED, BOUNCED는 항목이 없어도 흐린 한 줄(`QUEUED 0`)로 남고, 항목이 생기면 펼친다 |
| 3a | 그 밖 | 경고 카드는 지금처럼 있을 때만. POST-MERGE는 설정된 저장소만. 접힌 저장소는 지금처럼 한 줄 |
| — | 화면 | 문구는 영어, 색은 테마 토큰만. 좁은 패널(약 368px)에서 시간이 제목을 밀어내지 않는다(제목이 말줄임된다) |
| — | 작업 | 작업 수는 최소. chute로 머지한다. mod 작업은 `basic` |
| — | 캡처 | 사용자 창을 건드리지 않는 방법만. 안 되면 생략한다 |
| — | 순서 | board-polish(PR #63) 머지 뒤 시작한다. #63은 이미 main에 있다(`44db676`) |

## 3. 목업 (영어 화면, 패널 폭 약 368px)

```
│ ▾ vallythevally                   ● Queued · 2   │
│   NEEDS YOU                                   1  │
│   ┌──────────────────────────────────────────┐   │
│   │ #431 chute.toml timeout    waiting 12m ↗ │   │
│   │ Awaiting approval · Preflight passed …   │   │
│   │ …                                        │   │
│   └──────────────────────────────────────────┘   │
│   CURRENT BATCH                               0  │   ← 흐린 한 줄
│   QUEUED                                      2  │
│    1 #432 fix hoga sort ord…    waiting 3m   ↗   │
│    2 #433 bump deps             waiting 1m   ↗   │
│   BOUNCED                                     1  │
│   #429 e2e after… Verify failed · test   log ↗   │
│   t1rp · Retries 1/3                  12m ago    │
│   RECENT                                         │
│   #421 e2e after merge   1 bounce   18m 4s   ↗   │   ← hover: Merged 41m ago · 11:23
│   #420 fix login                     4m 12s  ↗   │
│   #418 bump deps                     6m 30s  ↗   │
│   + 2 more                                       │
│   TODAY  9 merged · 1 bounced · median 4m 12s ›  │
│                                                  │
│ ▾ chute                            ● All clear   │
│   NEEDS YOU                                   0  │   ← 빈 저장소도 네 줄이
│   CURRENT BATCH                               0  │     흐리게 남는다
│   QUEUED                                      0  │
│   BOUNCED                                     0  │
│   RECENT …                                       │
└──────────────────────────────────────────────────┘
```

## 4. 스스로 정한 결정과 그 이유

### 4.1 status에 더하는 필드 (본체)

| 결정 | 이유 |
|---|---|
| `recent_merged[]`에 `submitted_ms`(ms, 없으면 `null`)를 더한다. 값은 그 `merged` 이벤트의 `submitted_ms`다(`src/cmd/status.rs` `summarize`) | 기록에 이미 있는 값이다. history 형식을 바꾸지 않는다. 걸린 초를 따로 내지 않고 시각을 내는 것은 `queue[].submitted_ms`와 모양을 맞추고, mod가 `merged_ms`와 빼서 바로 쓸 수 있기 때문이다 |
| `queue[]`에 `bounced_ms`(ms, 없으면 `null`)를 더한다. 상태가 `bounced`인 항목에만, 그 PR의 가장 늦은 `bounced` 이벤트의 `at_ms`를 넣는다. 다른 상태는 `null`이다 | `last_failure`는 HEAD 바뀜 반송에 쓰이지 않고 앞선 반송의 것이 남을 수 있다(1절). history는 모든 반송을 남긴다. 반송 상태의 항목에서 가장 늦은 `bounced` 이벤트는 그 항목이 지금 반송 상태에 들어온 때다 |
| 두 필드 모두 Rust에서 `Option<EpochMs>`에 `#[serde(default)]`를 단다. 직렬화는 늘 하고(없으면 `null`) `skip_serializing_if`는 쓰지 않는다 | 이 필드가 없던 chute가 낸 status도 읽혀야 한다(`recent_merged`, `title`이 같은 방식이다). 늘 내보내야 `tests/fixtures/status-full.json`의 왕복 시험이 모양을 고정한다 |
| `bounced_ms`는 `status::view`가 이미 읽는 `history.events`에서 PR별 마지막 반송 시각을 모아 `ItemView`에 채운다(`ItemView::new`의 호출처는 `status::view` 하나다) | history를 한 번 더 읽지 않는다. 상태 파일(`state.json`)과 큐 상태 전이(`src/queue.rs`)는 건드리지 않는다 |
| `tests/fixtures/status-full.json`에 두 필드를 넣는다(최근 머지에 값, 반송 항목에 값, 다른 큐 항목에 `null`). `mod/chute/`는 같은 PR에서 그 필드를 쓴다 | `AGENTS.md`의 상태 JSON 규칙이다. 필드를 더하기만 하고 있는 필드의 이름과 뜻은 바꾸지 않는다 |
| README의 상태 JSON 설명에 두 필드를 한 줄로 적는다 | 다른 mod가 기댈 값이다 |

### 4.2 항목별 시간 (mod)

| 결정 | 이유 |
|---|---|
| QUEUED 행: `waiting 3m`을 태그(`Waiting on CI` 따위) 뒤, `↗` 앞에 둔다. NEEDS YOU 카드: 제목 줄의 `↗` 앞에 같은 `waiting 3m`을 둔다. 둘 다 `submitted_ms` 기준이다 | 요청 1a·1b다. 사람 차례(`needs_human`) 카드도 같은 구역이라 같은 기준을 쓴다(7절 1번) |
| 기다린 시간은 분 단위 새 함수(예: `formatWait(ms)`)로 쓴다: `<1m`, `3m`, `2h 5m`(분이 0이면 `2h`), `3d`(하루 넘으면 날만) | 초까지 쓰면 15초 poll마다 값이 튀고 매초 다시 그리지 않아 멈춘 것처럼 보인다. 분 단위는 poll 사이에 틀려 봐야 1분이다 |
| BOUNCED 행: `12m ago`(`formatAgo(now - bounced_ms)`)를 **둘째 줄**(`t1rp · Retries 1/3`) 오른쪽 끝에 둔다 | 첫 줄에는 이미 반송 이유(`Verify failed · test`)가 오른쪽에 있다. 368px에서 시간까지 첫 줄에 두면 제목이 거의 사라진다. 둘째 줄은 짧아 자리가 남는다 |
| RECENT 행: 지금의 `41m ago` 자리에 걸린 시간 `formatDuration((merged_ms - submitted_ms) / 1000)`(예: `18m 4s`)을 둔다. 그 칸의 tooltip은 `Merged 41m ago · 11:23`(현지 시:분) | 요청 1e(안 B)다. `formatDuration`은 TODAY의 `median 4m 12s`와 같은 함수라 두 숫자를 견줄 수 있다. 값은 머지 뒤 바뀌지 않아 초까지 써도 튀지 않는다 |
| `submitted_ms`가 없거나 `merged_ms`보다 크면(값이 이상하면) 걸린 시간 칸을 비우되 칸의 최소 폭과 tooltip은 남긴다. `bounced_ms`가 없으면 BOUNCED의 시간 칸도 비운다. 다른 값(예: `last_failure`)으로 짐작하지 않는다 | 요청 2다. 틀린 시간을 보이는 것보다 비우는 편이 낫다. tooltip이 남아 머지 시각은 여전히 볼 수 있다 |
| 옛 `recent`(batch 목록)에서 만든 최근 머지 행(`recent_merged`가 없던 chute)은 걸린 시간이 없으므로 칸을 비우고 tooltip만 둔다 | 위와 같다 |
| 시간 칸은 모두 `flex_none` + `whitespace_nowrap` + `text_right`, 글 크기 11, 색 `t.dim`이다. 제목은 지금처럼 `flex_1().min_w(0).truncate()`(카드 제목은 줄바꿈) | 시간이 제목을 밀어내지 않는다는 요청이다. board-polish 4.8의 숫자 칸 규칙과 같다 |
| 시간 계산(`waiting …`, `… ago`, 걸린 시간, tooltip 글)은 `board.js`가 행 데이터로 낸다(예: `wait`, `ago`, `took`, `tip`). `main.js`는 그리기만 한다 | `node --test`로 볼 수 있게 한다 |
| CURRENT BATCH와 POST-MERGE는 고치지 않는다 | 요청 1d다 |

### 4.3 빈 구역 머리 (mod)

| 결정 | 이유 |
|---|---|
| `repoBoard`의 `sections`에 NEEDS YOU(`human`), CURRENT BATCH(`batch`), QUEUED(`waiting`), BOUNCED(`bounced`)를 늘 넣는다. 비었으면 `items: []`다. `alerts`와 `unknown`은 지금처럼 있을 때만 넣는다 | 요청 3이다. 구역 목록을 쓰는 다른 계산(`overview`, 알림)은 `items.length`를 보므로 빈 목록이어도 결과가 같다 |
| 빈 구역은 `sectionTitle`과 같은 줄에 제목과 `0`을 두되 제목과 숫자를 모두 `t.dim`으로 그린다. 아래에 아무것도 두지 않는다 | 요청의 "흐린 한 줄 `QUEUED 0`"이다. 숫자 칸은 항목이 있는 구역과 같은 오른쪽 정렬이라 눈이 같은 자리를 본다 |
| 빈 CURRENT BATCH도 `CURRENT BATCH 0`이다 | 네 구역의 모양을 같게 한다. `idle`처럼 다른 글을 쓰면 한 줄만 다르다(7절 3번) |
| 허브에 닿지 않는 저장소(`Hub unreachable`)는 빈 머리를 그리지 않는다(지금처럼 `sections: []`) | 그 저장소는 큐를 모른다. `0`은 "비었다"로 읽혀 틀린 말이 된다 |
| 접힌 저장소는 지금처럼 머리 한 줄이다. 빈 머리 때문에 접기 규칙(board-polish 4.1)은 바뀌지 않는다 | 요청 3a다 |
| RECENT와 TODAY의 표시 조건은 바꾸지 않는다 | 요청이 든 네 구역이 아니다 |

### 4.4 작업 나누기

| 작업 | 내용 | 방식 | 방식의 이유 |
|---|---|---|---|
| `board-times` | 4.1 본체 필드 두 개와 fixture·시험, 4.2 항목별 시간, 4.3 빈 구역 머리, README 한 문단 | `basic` | status 보기(`chute status --json`의 출력)에 이미 기록된 값을 옮기는 읽기 전용 변경과 화면 조정이다. `state.json`·`history.jsonl`의 형식, 큐 상태 전이, 머지·push·검증 판정, 잠금, orch 코드는 건드리지 않는다 |

작업을 하나로 둔 이유: 요청은 본체(status 필드)와 mod를 따로 둘 것을 전제했지만, 본체 몫이 history 형식 변경이 아니라 보기에 필드 두 개를 옮기는 일로 줄었다(1절). `AGENTS.md`는 상태 JSON이 바뀌면 fixture와 `mod/chute/`를 같은 PR에서 고치라고 하고, mod가 그 필드를 바로 쓰므로 나누면 순차가 된다. 사용자 주간 사용량이 90%대라 작업 수를 줄였다. 작업이 하나라 병렬·순차 관계는 없다.

## 5. 고치는 곳과 건드리지 않는 곳

- 고친다: `src/model.rs`(`MergedView`, `ItemView`와 그 시험), `src/cmd/status.rs`(`summarize`, `view`와 그 시험), `tests/fixtures/status-full.json`, `mod/chute/board.js`, `mod/chute/main.js`, `mod/chute/board.test.js`, `README.md`(현황판 읽기, 상태 JSON 설명).
- 고치지 않는다: `src/queue.rs`, `src/store.rs`, `src/engine.rs`, `state.json`·`history.jsonl` 형식, `tests/fixtures/status-hub-unreachable.json`(허브 끊김 출력에는 큐와 최근 머지가 비어 있다. 왕복 시험이 깨지면 그때만 고친다), `mod/chute/mod.json`, 탭 막대 mod `tabs-chute`, 사용자의 `~/.config/herdr-app/` 아래 파일.
- `chute status`의 글 출력(사람이 읽는 표)은 바꾸지 않는다.

## 6. 범위 밖

- 사람 차례가 된 시각(`needs_human`에 들어온 때). history의 `needs_human` 이벤트로 낼 수 있지만 요청은 `submitted_ms`를 골랐다(7절 1번).
- 반송 뒤 다시 들어온 시각부터 세는 QUEUED 시간(7절 2번).
- `chute status` 글 출력에 시간 더하기.
- CURRENT BATCH·POST-MERGE의 시간 모양, RECENT 5건 상한(`RECENT_MERGED`).
- 상태 한 줄, 정렬, 접기, 알림 규칙(board-ux·board-en·board-polish에서 정함).

## 7. 사용자가 정해야 하는 결정

1. **사람 차례 카드의 시간.** NEEDS YOU 구역이라 승인 대기와 같이 처음 submit부터 센다(`waiting 2h`). 사람 차례가 된 뒤의 시간을 원하면 본체 필드 하나를 더해야 한다.
2. **반송 뒤 다시 들어온 PR의 QUEUED 시간.** 처음 submit부터 세므로 반송 전 시간까지 들어간다. 다시 들어온 때부터 세길 원하면 본체 필드가 하나 더 필요하다.
3. **빈 CURRENT BATCH의 표기.** 다른 세 구역과 같이 `CURRENT BATCH 0`이다. `idle` 같은 글을 원하면 말해 달라.
4. **BOUNCED 시간의 자리.** 좁은 패널에서 제목을 지키려고 첫 줄이 아닌 둘째 줄 오른쪽 끝에 둔다.
5. **RECENT 걸린 시간의 모양.** TODAY 중앙값과 같은 `18m 4s`(초까지)다. QUEUED처럼 분까지만(`18m`)을 원하면 말해 달라.
