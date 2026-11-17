# 현황판 영어화 플랜: chute 현황판 mod의 화면 문구를 영어로 바꾸고 실제 화면의 결함 셋을 고친다

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 board-ux(PR #59, `acd928d`)를 설치해 본 뒤 "그리고 언어는 영어로 해줘."라고 했다. 현황판 mod(`mod/chute/`)는 Herdr 오른쪽 패널(폭 약 368px)에서 기본 mod(`Files`, `Changes`, "Not a git repository")와 나란히 보이는데, 현황판만 한국어다.

같은 설치 화면(`/home/dev/.claude/jobs/3924a6bb/tmp/board-live.png`)에서 결함 셋이 보였다.

| 번호 | 결함 | 원인(코드에서 확인) |
|---|---|---|
| 1 | 최근 머지의 세 자리 PR 번호가 `#42` / `1`로 두 줄로 꺾인다 | `main.js` `recentView`가 번호 칸을 `.w(26)` 고정 폭으로 둔다 |
| 2 | "정상 · 대기 없음"과 머지 뒤 검사의 `✓`가 파란색이다 | `toneColor`가 정상(`ok`)을 `t.done`으로 잇고, `postVerifyView`가 `t.done`을 직접 쓴다. Herdr의 `done`은 "보지 않는 사이 에이전트가 끝났다"는 상태 색이고 프리셋에서 파랑이다(`themes/win98.json`도 `done: #0000ff`). 정상의 뜻이 아니다 |
| 3 | status-feed(PR #58) 이전에 머지된 PR이 "제목 없음"으로 줄줄이 나온다 | `recentView`가 제목이 비면 자리 문구 "제목 없음"을 넣는다. 옛 history의 `merged` 사건에는 제목이 없다 |

오른쪽 위 탭 막대의 `chute` 버튼은 사용자 개인 mod `tabs-chute`이고 이 저장소 밖이다. 손대지 않는다.

## 2. 확정된 결정

요청에 적힌 것이다.

| 항목 | 결정 |
|---|---|
| 영어로 바꿀 곳 | 현황판 mod의 모든 화면 문구: 상태 한 줄, 구역 제목, 버튼, 경고 카드, toast, 빈 상태, 오류, 시간 표시("3분 전" → "3m ago"), 단위 |
| 바꾸지 않을 곳 | chute 본체(Rust)의 CLI 출력과 `chute --skill` |
| 본체 문장 | mod가 chute 출력의 한국어 문장을 그대로 보이는 곳은 목록으로 보고한다. 코드값에서 영어로 바꿀 수 있는 것은 mod에서 바꾼다. 본체 변경이 꼭 필요하면 범위 밖으로 적는다 |
| 문구 톤 | 짧고 자연스럽게, Herdr 기본 mod(Files/Changes)의 톤 |
| 한국어 전환 | 만들지 않는다 |
| 결함 1 | 번호 칸이 번호 길이에 맞게 넓어지고 줄바꿈하지 않는다 |
| 결함 2 | 의미→토큰 함수를 확인해 고친다 |
| 결함 3 | 제목이 없으면 자리 문구 없이 번호만 보이거나 흐린 짧은 표시로 줄인다 |
| 탭 막대 `chute` 버튼 | 손대지 않는다 |
| 머지와 방식 | chute로 머지한다. 화면 작업이라 `basic` |
| 완료 확인 | mod 시험, `herdr-app check`, 한글 grep(남는 것은 본체 출력을 그대로 보이는 곳뿐이고 목록으로 적는다) |
| 캡처 | 사용자 창을 뺏지 않는 방법으로만 한다. 못 하면 생략하고 적는다 |

## 3. 스스로 정한 결정과 그 이유

| 결정 | 이유 |
|---|---|
| 정상(`ok`)의 색을 `ansi2`(초록)로 바꾼다. board-ux 플랜 4.2의 "정상 = `done`"을 고친다 | Herdr의 `done`은 정상이 아니라 "안 본 사이 끝남"이고 파랗다(1절). 기본 mod는 긍정을 `ansi2`로 그린다(`changes`의 `+`, `spaces`의 연결 점). 실패는 이미 `ansi1`이라 짝이 맞는다 |
| 의미→토큰 표를 `board.js`의 순수 함수(예: `toneToken(tone)` → 토큰 이름)로 옮기고 `main.js`는 `theme()[이름]`만 읽는다. `main.js`는 `t.done`을 직접 쓰지 않는다 | 결함 2가 다시 생기지 않게 `node --test`로 볼 수 있어야 한다. 지금 표는 `main.js`에 있어서 시험할 수 없다 |
| 제목이 없는 최근 머지는 번호만 보이고 제목 칸은 비운다. 자리 문구도 흐린 표시도 두지 않는다 | 같은 흐린 문구가 다섯 줄 반복되면 정보 없이 눈만 끈다. 줄을 누르면 GitHub PR이 열리므로 제목은 거기서 본다 |
| PR 번호 칸은 고정 폭 대신 `flex_none` + 줄바꿈 금지 + 최소 폭(정렬용)으로 둔다. `#번호`를 그리는 다른 곳(batch, 대기, 반송, 카드 제목)도 줄바꿈 금지를 확인한다 | 네 자리 번호에서도 같은 일이 생기지 않게 한다 |
| 경고(`warnings[]`)는 `kind`를 영어 제목으로 바꾸고(예: `config` → "Can't read config"), 본체의 한국어 `message`는 그 아래 작은 글로 그대로 둔다. 모르는 `kind`는 "Warning" | 코드값으로 영어 제목을 낼 수 있고, 본체 문장에는 PR 번호나 경로처럼 쓸모 있는 값이 있다 |
| 본체가 낸 한국어 문장 다섯 가지는 번역하지 않고 그대로 보인다(4절) | mod가 자유 문장을 번역하면 틀리거나 정보를 잃는다. 본체 변경은 요청 범위 밖이다 |
| `AGENTS.md`의 "사용자에게 보이는 글은 한국어" 규칙에 "`mod/chute/`의 화면 문구는 영어다(2026-11-17 사용자 결정)"를 한 줄 더한다 | 규칙이 그대로면 다음 작업자가 규칙을 따라 한국어로 되돌린다. 문서·보고서·CLI는 한국어 그대로다 |
| README에서 현황판 화면 문구를 그대로 옮긴 곳(209줄의 "허브에 닿지 않는다")만 영어 문구로 맞추고, "현황판 읽기"에 "화면 문구는 영어이고 본체가 낸 문장은 한국어로 보인다"를 한 문장 더한다 | 설명서가 화면에 없는 문구를 가리키지 않게 한다. 설명서 자체는 한국어 규칙을 따른다 |
| 캡처는 사용자 Herdr 창의 space·tab 선택을 바꾸지 않는 방법으로만 한다. `herdr-app shot`은 지금 선택된 화면만 찍으므로 사용자 창에서 `tab.new`/`tab.select`로 선택을 옮기는 방법은 쓰지 않는다. 임시 `HERDR_APP_HOME`의 별도 Herdr 창으로 사용자 화면을 가리지 않고 찍을 수 있으면 그렇게 하고, 아니면 생략하고 보고서에 적는다 | 요청의 제약. board-ux 때 쓴 방법(사용자 창에 tab을 열고 선택을 옮김)은 창을 뺏는다 |
| 작업은 하나다 | 바뀌는 곳이 모두 `board.js`와 `main.js`의 문구와 그 시험이다. 나누면 같은 줄을 두 작업이 고친다 |

### 3.1 영어 문구 기준

작업자는 아래 용어를 일관되게 쓴다. 문장은 sentence case, 끝에 마침표를 찍지 않는다(설명 문장인 안내 hint만 마침표). 단위는 `45s`, `4m 12s`, `1h 5m`, 상대 시각은 `just now`, `3m ago`, `2h ago`, `3d ago`, 경과는 `for 14m`. 복수는 수와 맞춘다(`1 bounce`, `2 bounces`). 좁은 폭에 맞추려 낱말을 줄일 수 있지만 용어는 바꾸지 않는다.

| 뜻 | 영어 |
|---|---|
| 사람 차례 | Needs you |
| 승인 대기 | Awaiting approval |
| 대기 / CI 대기 | Queued / Waiting on CI |
| 반송 | Bounced, bounce |
| 검증 중 / 단계 n/m | Verifying / `test 2/3` |
| 지금 batch | Current batch |
| 평소 / 평소보다 길다 | usually 6m 28s / Slower than usual |
| 머지 뒤 검사 | Post-merge check |
| 최근 머지 | Recently merged |
| 오늘 | Today (`9 merged · 1 bounced · median 4m`, 펼치면 `Total verify time`, `(sum of 10 batches)`, 만성 칩 `chronic`) |
| 멈춤 | Stopped (`Queue stopped: base commit failed verification`) |
| 데몬 꺼짐 | Daemon is off (안내 "Run `chute up` to start it.") |
| 허브 | hub (`hub desk`, `this PC`, `Hub unreachable`, `Retrying every 5s`) |
| 정상 | All clear |
| PC 자리 기다림 | Waiting for a slot |
| 버튼 | Approve, Retry, Drop, Working…, Report ›, Log ›, Close, Collapse, Expand, Open panel |
| 보고서·로그 | Report, Log |
| 재시도 | Retries 1/3 |
| 에이전트 없음 / 종료됨 | No agent / exited |
| 오류 | Couldn't read status, Can't run chute, No repositories, Loading, Exit code 3 |

상태 한 줄의 우선순위와 색의 뜻은 board-ux 플랜 4.2 그대로이고 문구만 바뀐다. 정상의 색만 3절대로 `ansi2`다.

| 순서 | 문구 |
|---|---|
| 1 | Hub unreachable |
| 2 | Stopped · base failed verification |
| 3 | Post-merge check failed · e2e |
| 4 | Daemon is off |
| 5 | Needs you · 2 |
| 6 | Verifying · test 2/3 (단계 수가 없으면 `Verifying · test`, 자리를 기다리면 `Waiting for a slot`) |
| 7 | Queued · 3 |
| 8 | All clear |

## 4. 본체 출력을 그대로 보이는 곳 (한국어가 남는 곳)

mod 코드(`board.js`, `main.js`)에는 한글 리터럴이 하나도 남지 않는다. 화면에 한국어가 보일 수 있는 것은 다음 값뿐이다.

| 값 | 어디에 보이나 | 영어로 바꾸지 않는 이유 |
|---|---|---|
| `warnings[].message` | 경고 카드의 작은 글(제목은 `kind`의 영어) | 본체의 자유 문장. PR 번호, 경로 같은 값이 섞여 있다 |
| `hub.error` | "Hub unreachable" 아래 이유 | 본체 문장에 ssh·hc 출력이 섞여 있다 |
| `last_failure.summary` | 사람 차례 카드의 실패 요약(예: `test 단계 실패: …`) | 본체 `feedback.rs`가 만든 문장 |
| `chute status`나 `approve`/`retry`/`drop`이 실패했을 때 stderr 첫 줄 | 읽기 실패 카드, 사람 차례 카드의 오류 줄 | 본체 CLI 오류 문장 |
| 데이터: PR 제목, 에이전트 이름, 단계·시험 이름, 보고서와 로그 파일 내용 | 곳곳 | 화면 문구가 아니라 사용자의 데이터 |

이 문장들을 영어로 내려면 본체가 문장 대신 코드값을 내거나 영어 문장을 함께 내야 한다. 이번 범위 밖이다(6절).

## 5. 작업 나누기

| 작업 | 내용 | 방식 | 방식의 이유 |
|---|---|---|---|
| `board-en` | mod 문구 영어화, 결함 1~3, 시험, `AGENTS.md` 한 줄, README 두 곳 | `basic` | 문구와 화면 조정이다. 머지·상태 저장·잠금을 건드리지 않고 `status` 출력을 읽기만 한다 |

작업이 하나라 병렬·순차 관계는 없다.

## 6. 범위 밖

- chute 본체의 문장을 코드값으로 바꾸거나 영어로 내는 일(4절의 표). 원하면 따로 요청한다.
- `chute --skill`, CLI 출력, 보고서 파일의 언어.
- README "현황판 읽기"의 순서 설명이 board-ux 이후 화면과 맞지 않는 점. 이번에는 문구를 옮긴 곳만 고친다.
- 탭 막대의 `chute` 버튼(`tabs-chute`, 저장소 밖), 다른 Herdr mod, 레이아웃, 테마, `seen.json`.
- 한국어·영어 전환 옵션.

## 7. 사용자가 정해야 하는 결정

1. **정상 색.** 이 플랜은 정상을 초록(`ansi2`)으로 둔다. 정상은 눈에 띌 필요가 없다고 보면 점만 초록, 글은 흐리게(`muted`) 둘 수도 있다. 기본은 점과 글 모두 초록이다.
2. **본체의 한국어 문장이 영어 화면에 섞이는 것.** 경고의 작은 글, 허브 오류, 실패 요약, 명령 오류는 한국어로 보인다(4절). 받아들이면 그대로 가고, 영어로 원하면 본체 작업을 따로 연다.
3. **`AGENTS.md`에 예외 한 줄을 더하는 것.** 원하지 않으면 승인 때 말해 달라. 그러면 PR 본문에만 적는다.
